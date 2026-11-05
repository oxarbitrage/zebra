//! Assembles known-hash list chunk files from a synced node.
//!
//! Sweeps every block hash and serialized block size from a local, fully
//! synced node over JSON-RPC, checks every swept hash at a checkpoint height
//! against the network's checkpoint list, then emits the chunk files and the
//! `KnownHashListSpec` constant that pins them.
//!
//! Typical usage, against a synced local node:
//!
//! ```sh
//! known-hashes --network mainnet --out-dir assets/
//! known-hashes --network mainnet --out-dir assets/ --emit-chunks --end-height 3373206
//! ```
//!
//! For all options please refer to the program help: `known-hashes --help`

use clap::Parser;
use color_eyre::{
    eyre::{ensure, eyre, Result},
    Help,
};

use zebra_chain::{
    block,
    parameters::{
        known_hashes::{size_hint, KnownHashListSpec, HASHES_PER_CHUNK},
        Network,
    },
};
use zebra_utils::init_tracing;

mod args;
mod emit;
mod source;
mod sweep;

#[cfg(test)]
mod tests;

use args::Args;
use source::{BlockSource, RpcBlockSource};
use sweep::{verify_anchors, SweepFiles};

/// The number of blocks below the node tip that the sweep stops at by
/// default, so it never records blocks that could still be reorged away.
const CONFIRMATION_MARGIN: u32 = 100;

/// Process entry point for `known-hashes`
#[tokio::main]
// This is an interactive maintenance tool: results go to stdout, and progress
// goes to stderr, following the other `zebra-utils` binaries.
#[allow(clippy::print_stdout, clippy::print_stderr)]
async fn main() -> Result<()> {
    init_tracing();
    color_eyre::install()?;

    let args = Args::parse();
    let network = &args.network;

    // The network part matches the `main-checkpoints.txt` and
    // `test-checkpoints.txt` file names.
    let file_prefix = match network {
        Network::Mainnet => "main-known-hashes",
        Network::Testnet(_) => "test-known-hashes",
    };
    let network_name = network.to_string().to_lowercase();

    std::fs::create_dir_all(&args.out_dir)?;
    let hash_path = args
        .out_dir
        .join(format!("swept-hashes-{network_name}.bin"));
    let sizes_path = args.sizes_file.clone().unwrap_or_else(|| {
        args.out_dir
            .join(format!("block-sizes-{network_name}.u32le"))
    });

    let mut files = SweepFiles::open(&hash_path, &sizes_path)?;

    if args.emit_chunks {
        let max_height = args
            .end_height
            .ok_or_else(|| eyre!("--emit-chunks requires --end-height"))
            .with_suggestion(|| "pass the checkpoint height the published list should end at")?;

        let constant_block = run_emit(network, &files, max_height, &args, file_prefix)?;
        println!("{constant_block}");
    } else {
        let source = RpcBlockSource::new(args.rpc_addr);

        let end_height = match args.end_height {
            Some(height) => height.0,
            None => {
                let tip = source.tip_height().await.with_suggestion(|| {
                    "is the node running, and is --rpc-addr its JSON-RPC address?"
                })?;

                tip.checked_sub(CONFIRMATION_MARGIN).ok_or_else(|| {
                    eyre!(
                        "node tip {tip} is below the {CONFIRMATION_MARGIN} block \
                         confirmation margin: wait for the node to sync further",
                    )
                })?
            }
        };

        sweep::run_sweep(
            &source,
            network,
            &mut files,
            args.start_height.0,
            end_height,
            args.concurrency,
        )
        .await?;
    }

    Ok(())
}

/// Converts the completed sweep in `files` into chunk files, verifies them,
/// and returns the `KnownHashListSpec` constant block.
///
/// Fails if any height up to `max_height` is missing from the sweep, if any
/// checkpoint mismatches, or if `max_height` isn't a checkpoint height.
//
// Progress goes to stderr because this is an interactive maintenance tool.
#[allow(clippy::print_stderr)]
fn run_emit(
    network: &Network,
    files: &SweepFiles,
    max_height: block::Height,
    args: &Args,
    file_prefix: &str,
) -> Result<String> {
    let checkpoints = network.checkpoint_list();
    ensure!(
        checkpoints.contains(max_height),
        "--end-height {max_height:?} is not a {network} checkpoint height: \
         the list must end at a checkpoint",
    );

    let hashes = files.complete_hashes(max_height.0)?;
    let sizes = files.complete_sizes(max_height.0)?;

    let (verified, missing) = verify_anchors(network, files, max_height.0)?;
    ensure!(
        missing == 0,
        "{missing} checkpoints at or below height {max_height:?} are not swept yet",
    );

    let blocks = hashes
        .into_iter()
        .zip(sizes)
        .enumerate()
        .map(|(height, (hash, size))| {
            let hint = size_hint(size.into())
                .ok_or_else(|| eyre!("invalid block size {size} at height {height}"))?;
            Ok((block::Hash(hash), hint))
        })
        .collect::<Result<Vec<_>>>()?;

    let chunk_hashes = emit::emit_chunks(&args.out_dir, file_prefix, &blocks, HASHES_PER_CHUNK)?;
    let chunk_hashes: Vec<&str> = chunk_hashes.iter().map(String::as_str).collect();

    let spec = KnownHashListSpec {
        max_height,
        chunk_blocks: HASHES_PER_CHUNK,
        file_prefix,
        chunk_hashes: &chunk_hashes,
    };
    emit::verify_emitted(&args.out_dir, spec, checkpoints)?;

    eprintln!(
        "emitted and verified {} chunks covering {} blocks in {} ({verified} checkpoints checked)",
        chunk_hashes.len(),
        blocks.len(),
        args.out_dir.display(),
    );

    let const_prefix = match network {
        Network::Mainnet => "MAINNET",
        Network::Testnet(_) => "TESTNET",
    };

    Ok(emit::spec_constant_block(const_prefix, &spec))
}
