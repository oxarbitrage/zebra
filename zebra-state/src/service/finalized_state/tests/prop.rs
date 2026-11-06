//! Randomised property tests for the finalized state.

use std::env;

use tokio::sync::oneshot;

use zebra_chain::{
    amount::{Amount, NonNegative},
    block::Height,
    parameters::{
        testnet::{ConfiguredActivationHeights, ParametersBuilder},
        Network, NetworkUpgrade,
    },
    LedgerState,
};
use zebra_test::prelude::*;

use crate::{
    config::Config,
    service::{arbitrary::PreparedChain, finalized_state::FinalizedState, ChainTipBlock},
    tests::FakeChainHelper,
    CheckpointVerifiedBlock,
};

const DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES: u32 = 1;

#[test]
fn blocks_with_v5_transactions() -> Result<()> {
    let _init_guard = zebra_test::init();
    proptest!(ProptestConfig::with_cases(env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES)),
        |((chain, count, network, _history_tree) in PreparedChain::default())| {
            let mut state = FinalizedState::new(&Config::ephemeral(), &network).expect("opening an ephemeral database should succeed");
            let mut height = Height(0);
            // use `count` to minimize test failures, so they are easier to diagnose
            for block in chain.iter().take(count) {
                let checkpoint_verified = CheckpointVerifiedBlock::from(block.block.clone());
                let (hash, _) = state.commit_finalized_direct(
                    checkpoint_verified.into(),
                    None,
                    "blocks_with_v5_transactions test"
                ).unwrap();
                prop_assert_eq!(Some(height), state.finalized_tip_height());
                prop_assert_eq!(hash, block.hash);
                height = Height(height.0 + 1);
            }
    });

    Ok(())
}

/// Test if committing blocks from all upgrades work correctly, to make
/// sure the contextual validation done by the finalized state works.
/// Also test if a block with the wrong commitment is correctly rejected.
#[test]
#[allow(clippy::print_stderr)]
fn all_upgrades_and_wrong_commitments_with_fake_activation_heights() -> Result<()> {
    let _init_guard = zebra_test::init();

    let (network, initial_nsm_value_balance) = fake_activation_heights_network();
    let ledger_strategy =
        LedgerState::genesis_strategy(Some(network), NetworkUpgrade::Nu5, None, false);

    // Use no_shrink() because we're ignoring _count and there is nothing to actually shrink.
    proptest!(ProptestConfig::with_cases(env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES)),
        |((chain, _count, network, _history_tree) in PreparedChain::default().with_ledger_strategy(ledger_strategy).with_valid_commitments().no_shrink())| {

            let mut state = FinalizedState::new(&Config::ephemeral(), &network).expect("opening an ephemeral database should succeed");
            let mut height = Height(0);
            let mut note_commitment_trees = None;
            let heartwood_height = NetworkUpgrade::Heartwood.activation_height(&network).unwrap();
            let heartwood_height_plus1 = (heartwood_height + 1).unwrap();
            let nu5_height = NetworkUpgrade::Nu5.activation_height(&network).unwrap();
            let nu5_height_plus1 = (nu5_height + 1).unwrap();

            let mut failure_count = 0;
            for block in chain.iter() {
                let block_hash = block.hash;
                let current_height = block.block.coinbase_height().unwrap();
                // For some specific heights, try to commit a block with
                // corrupted commitment.
                match current_height {
                    h if h == heartwood_height ||
                        h == heartwood_height_plus1 ||
                        h == nu5_height ||
                        h == nu5_height_plus1 => {
                            let block = block.block.clone().set_block_commitment([0x42; 32]);
                            let checkpoint_verified = CheckpointVerifiedBlock::from(block);
                            let (rsp_tx, mut rsp_rx) = oneshot::channel();
                            let error = state.commit_finalized(
                                (checkpoint_verified, rsp_tx),
                                note_commitment_trees.clone(),
                            ).expect_err("Must fail commitment check");
                            let response_error = rsp_rx.try_recv().unwrap().unwrap_err();
                            prop_assert_eq!(error.inner(), response_error.inner());
                            prop_assert_eq!(state.finalized_tip_height(), Some(Height(height.0 - 1)));
                            failure_count += 1;
                        },
                    _ => {},
                }
                let checkpoint_verified = CheckpointVerifiedBlock::from(block.block.clone());
                let expected_tip = ChainTipBlock::from(checkpoint_verified.clone());
                let (rsp_tx, mut rsp_rx) = oneshot::channel();
                let (tip, trees) = state.commit_finalized(
                    (checkpoint_verified, rsp_tx),
                    note_commitment_trees.take(),
                ).unwrap();
                prop_assert_eq!(tip, expected_tip);
                prop_assert_eq!(rsp_rx.try_recv().unwrap().unwrap(), block_hash);
                prop_assert_eq!(Some(height), state.finalized_tip_height());
                note_commitment_trees = Some(trees);
                height = Height(height.0 + 1);
            }
            // Make sure the failure path was triggered
            prop_assert_eq!(failure_count, 4);

            // The finalized state tracked the NSM value balance from NU7 activation: seeded, then
            // debited by each block's additional subsidy and credited with its ZIP 235 fee
            // contribution.
            {
                use zebra_chain::{
                    parameters::subsidy::{additional_block_subsidy, nsm_fee_contribution},
                    transparent::utxos_from_ordered_utxos,
                };

                let utxos = utxos_from_ordered_utxos(
                    chain.iter().flat_map(|block| block.new_outputs.iter().map(|(outpoint, utxo)| (*outpoint, utxo.clone()))),
                );

                let nu7_height = NetworkUpgrade::Nu7.activation_height(&network).unwrap();
                let tip_height = state.finalized_tip_height().unwrap();
                prop_assert!(tip_height >= nu7_height, "the chain must reach NU7");

                let mut expected = initial_nsm_value_balance;
                for height in (nu7_height.0..=tip_height.0).map(Height) {
                    let fees = chain[height.0 as usize].block.transaction_fees(&utxos).expect("valid fees");

                    expected = (expected - additional_block_subsidy(height, &network, expected))
                        .expect("the balance never goes negative");
                    expected = (expected + nsm_fee_contribution(height, &network, fees))
                        .expect("the balance stays a valid amount");
                }

                prop_assert_eq!(state.finalized_value_pool().nsm_amount(), expected);
                prop_assert!(expected > zebra_chain::amount::Amount::<zebra_chain::amount::NonNegative>::zero());
            }
    });

    Ok(())
}

/// Returns a network with every network upgrade activated within the first
/// `MAX_PARTIAL_CHAIN_BLOCKS` blocks, and its initial NSM value balance.
fn fake_activation_heights_network() -> (Network, Amount<NonNegative>) {
    let network = ParametersBuilder::default()
        .with_slow_start_interval(zebra_chain::block::Height::MIN)
        .with_activation_heights(ConfiguredActivationHeights {
            // These are dummy values. The particular values don't matter much,
            // as long as the nu7 one is smaller than the chains being generated
            // (MAX_PARTIAL_CHAIN_BLOCKS) to make sure all upgrades are exercised
            // in the test below. (The test will fail if that does not happen.)
            before_overwinter: Some(1),
            overwinter: Some(10),
            sapling: Some(15),
            blossom: Some(20),
            heartwood: Some(25),
            canopy: Some(30),
            nu5: Some(35),
            nu6: Some(40),
            nu6_1: Some(45),
            nu6_2: Some(47),
            nu6_3: Some(48),
            nu7: Some(50),
        })
        .expect("failed to set activation heights")
        .with_nsm_reissuance_height(Height(50))
        // These short chains have no historical funding-stream deposits to disburse.
        .clear_funding_streams()
        .with_lockbox_disbursements(vec![]);

    // Seed the NSM value balance at NU7 activation, so that committing blocks past it reissues
    // from the balance and debits it.
    let initial_nsm_value_balance =
        zebra_chain::amount::Amount::try_from(10 * zebra_chain::amount::COIN)
            .expect("valid amount");
    let network = network.with_initial_nsm_value_balance(initial_nsm_value_balance);

    let network = network
        .to_network()
        .expect("failed to build configured network");

    (network, initial_nsm_value_balance)
}

/// Test that the finalized tip cache carried between checkpoint commits always matches the
/// database, across every network upgrade and after failed commits.
#[test]
fn checkpoint_tip_cache_matches_database() -> Result<()> {
    let _init_guard = zebra_test::init();

    let (network, _) = fake_activation_heights_network();
    let ledger_strategy =
        LedgerState::genesis_strategy(Some(network), NetworkUpgrade::Nu5, None, false);

    proptest!(ProptestConfig::with_cases(env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES)),
        |((chain, _count, network, _history_tree) in PreparedChain::default().with_ledger_strategy(ledger_strategy).with_valid_commitments().no_shrink())| {
            // Skip format upgrades, so the upgrade task can't mark them as finished on its own.
            let mut state = FinalizedState::new_with_debug(&Config::ephemeral(), &network, true, false)
                .expect("opening an ephemeral database should succeed");
            let mut note_commitment_trees = None;
            let mut tip_cache = None;
            let heartwood_height = NetworkUpgrade::Heartwood.activation_height(&network).unwrap();
            let nu5_height = NetworkUpgrade::Nu5.activation_height(&network).unwrap();

            for block in chain.iter() {
                let current_height = block.block.coinbase_height().unwrap();

                // A failed commit drops the cache, like the block write task does.
                if current_height == heartwood_height || current_height == nu5_height {
                    let block = block.block.clone().set_block_commitment([0x42; 32]);
                    let (rsp_tx, _rsp_rx) = oneshot::channel();
                    state.commit_checkpoint_verified(
                        (CheckpointVerifiedBlock::from(block), rsp_tx),
                        note_commitment_trees.clone(),
                        tip_cache.take(),
                    ).expect_err("Must fail commitment check");
                }

                let (rsp_tx, _rsp_rx) = oneshot::channel();
                let (tip, trees, new_tip_cache) = state.commit_checkpoint_verified(
                    (CheckpointVerifiedBlock::from(block.block.clone()), rsp_tx),
                    note_commitment_trees.take(),
                    tip_cache.take(),
                ).unwrap();
                note_commitment_trees = Some(trees);
                prop_assert_eq!(tip.hash, block.hash);

                if current_height.is_min() {
                    // There's no cache while format upgrades might be running.
                    prop_assert!(new_tip_cache.is_none());
                    state.db.mark_finished_format_upgrades();
                } else {
                    let new_tip_cache = new_tip_cache.expect("format upgrades have finished");
                    prop_assert_eq!(new_tip_cache.tip, (current_height, block.hash));
                    new_tip_cache.assert_matches_db(&state.db);
                    tip_cache = Some(new_tip_cache);
                }
            }

            let tip_cache = tip_cache.expect("the chain has blocks after genesis");
            prop_assert!(tip_cache.history_tree.is_some(), "the chain must reach Heartwood");
            prop_assert!(!tip_cache.value_pool.transparent_amount().is_zero());
    });

    Ok(())
}

/// Test that the overlay of recently written outputs and address balances carried between
/// checkpoint commits always matches the database, and doesn't change what is written, with
/// budgets small enough to evict entries every block, and after failed commits.
///
/// Debug builds also check every overlay hit against the database during the commit.
#[test]
fn checkpoint_commit_overlay_matches_database() -> Result<()> {
    use crate::service::finalized_state::STATE_COLUMN_FAMILIES_IN_CODE;

    let _init_guard = zebra_test::init();

    let (network, _) = fake_activation_heights_network();
    let ledger_strategy =
        LedgerState::genesis_strategy(Some(network), NetworkUpgrade::Nu5, None, false);

    proptest!(ProptestConfig::with_cases(env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES)),
        |((chain, _count, network, _history_tree) in PreparedChain::default().with_ledger_strategy(ledger_strategy).with_valid_commitments().no_shrink(),
          budget_bytes in 1..20_000usize)| {
            // Skip format upgrades, so the upgrade task can't mark them as finished on its own.
            let mut state = FinalizedState::new_with_debug(&Config::ephemeral(), &network, true, false)
                .expect("opening an ephemeral database should succeed");
            state.checkpoint_commit_cache_bytes = budget_bytes;

            // The same chain, committed without an overlay.
            let mut reference_state = FinalizedState::new_with_debug(&Config::ephemeral(), &network, true, false)
                .expect("opening an ephemeral database should succeed");
            reference_state.checkpoint_commit_cache_bytes = 0;

            let mut note_commitment_trees = None;
            let mut tip_cache = None;
            let mut reference_note_commitment_trees = None;
            let mut reference_tip_cache = None;
            let mut lookups = Vec::new();

            let heartwood_height = NetworkUpgrade::Heartwood.activation_height(&network).unwrap();
            let nu5_height = NetworkUpgrade::Nu5.activation_height(&network).unwrap();

            for block in chain.iter() {
                let current_height = block.block.coinbase_height().unwrap();

                // A failed commit drops the overlay, like the block write task does.
                if current_height == heartwood_height || current_height == nu5_height {
                    let block = block.block.clone().set_block_commitment([0x42; 32]);
                    let (rsp_tx, _rsp_rx) = oneshot::channel();
                    let failed_tip_cache: Option<crate::service::finalized_state::CheckpointTipCache> = tip_cache.take();
                    if let Some(overlay) = failed_tip_cache.as_ref().and_then(|cache| cache.overlay.as_ref()) {
                        lookups.push(overlay.lookups());
                    }
                    state.commit_checkpoint_verified(
                        (CheckpointVerifiedBlock::from(block), rsp_tx),
                        note_commitment_trees.clone(),
                        failed_tip_cache,
                    ).expect_err("Must fail commitment check");
                }

                let (rsp_tx, _rsp_rx) = oneshot::channel();
                let (tip, trees, new_tip_cache) = state.commit_checkpoint_verified(
                    (CheckpointVerifiedBlock::from(block.block.clone()), rsp_tx),
                    note_commitment_trees.take(),
                    tip_cache.take(),
                ).unwrap();
                note_commitment_trees = Some(trees);
                prop_assert_eq!(tip.hash, block.hash);

                let (rsp_tx, _rsp_rx) = oneshot::channel();
                let (reference_tip, trees, new_reference_tip_cache) = reference_state.commit_checkpoint_verified(
                    (CheckpointVerifiedBlock::from(block.block.clone()), rsp_tx),
                    reference_note_commitment_trees.take(),
                    reference_tip_cache.take(),
                ).unwrap();
                reference_note_commitment_trees = Some(trees);
                prop_assert_eq!(reference_tip.hash, block.hash);

                // The overlay doesn't change anything written to the database.
                for cf_name in STATE_COLUMN_FAMILIES_IN_CODE {
                    prop_assert_eq!(
                        state.db.raw_column_family_contents(cf_name),
                        reference_state.db.raw_column_family_contents(cf_name),
                        "column family {} differs at height {:?}",
                        cf_name,
                        current_height,
                    );
                }

                if current_height.is_min() {
                    // There's no overlay while format upgrades might be running.
                    prop_assert!(new_tip_cache.is_none());
                    state.db.mark_finished_format_upgrades();
                    reference_state.db.mark_finished_format_upgrades();
                } else {
                    let new_tip_cache = new_tip_cache.expect("format upgrades have finished");
                    new_tip_cache.assert_overlay_matches_db(&state.db);

                    let overlay = new_tip_cache.overlay.as_ref().expect("the overlay is enabled");
                    prop_assert!(overlay.bytes() <= budget_bytes);

                    let new_reference_tip_cache = new_reference_tip_cache.expect("format upgrades have finished");
                    prop_assert!(new_reference_tip_cache.overlay.is_none(), "a zero budget disables the overlay");

                    tip_cache = Some(new_tip_cache);
                    reference_tip_cache = Some(new_reference_tip_cache);
                }
            }

            if let Some(overlay) = tip_cache.as_ref().and_then(|cache| cache.overlay.as_ref()) {
                lookups.push(overlay.lookups());
            }
            let lookups: u64 = lookups
                .iter()
                .map(|lookups| lookups.output_hits + lookups.output_misses + lookups.balance_hits + lookups.balance_misses)
                .sum();
            prop_assert!(lookups > 0, "the overlay must be used");
    });

    Ok(())
}
