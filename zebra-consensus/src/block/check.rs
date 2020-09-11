//! Consensus check functions

use super::*;
use zebra_chain::block::Block;

use std::convert::TryInto;
use zebra_chain::parameters::{Network, NetworkUpgrade::*};

use crate::parameters::Params;

/// Check that there is exactly one coinbase transaction in `Block`, and that
/// the coinbase transaction is the first transaction in the block.
///
/// "The first (and only the first) transaction in a block is a coinbase
/// transaction, which collects and spends any miner subsidy and transaction
/// fees paid by transactions included in this block." [§3.10][3.10]
///
/// [3.10]: https://zips.z.cash/protocol/protocol.pdf#coinbasetransactions
pub fn is_coinbase_first(block: &Block) -> Result<(), Error> {
    let first = block
        .transactions
        .get(0)
        .ok_or("block has no transactions")?;
    let mut rest = block.transactions.iter().skip(1);
    if !first.is_coinbase() {
        return Err("first transaction must be coinbase".into());
    }
    if rest.any(|tx| tx.contains_coinbase_input()) {
        return Err("coinbase input found in non-coinbase transaction".into());
    }
    Ok(())
}

/// [3.9]: https://zips.z.cash/protocol/protocol.pdf#subsidyconcepts
pub fn is_subsidy_correct(block: &Block) -> Result<(), Error> {
    let height = block.coinbase_height().unwrap();

    let coinbase = block.transactions.get(0).ok_or("no coinbase transaction")?;
    let outputs = coinbase.outputs();

    // Todo: we need the network here.
    let network = Network::Mainnet;

    let canopy_height = Canopy.activation_height(network).ok_or("no canopy")?;
    if height >= canopy_height {
        // dont validate canopy yet
        return Ok(());
    }

    // validate founders reward and miner subsidy
    if height > block::Height(0) && height <= block::Height(Params::LAST_FOUNDER_REWARD_HEIGHT) {
        let block_subsidy = subsidies::block_subsidy(height, Network::Mainnet);
        let miner_subsidy = subsidies::miner_subsidy(height, Network::Mainnet);
        let mut valid_founders: bool = false;
        let mut valid_miner: bool = false;
        for o in outputs {
            let value: i64 = o.value.try_into().unwrap();
            if value == block_subsidy as i64 / 5 {
                valid_founders = true;
            } else if value == miner_subsidy as i64 {
                valid_miner = true;
            }
        }
        if valid_founders && valid_miner {
            return Ok(());
        }
    }
    Err("error in the validation")?
}
