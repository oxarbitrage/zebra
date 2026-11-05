//! known-hashes arguments
//!
//! For usage please refer to the program help: `known-hashes --help`

use std::{net::SocketAddr, path::PathBuf};

use clap::Parser;

use zebra_chain::{block::Height, parameters::Network};

/// known-hashes arguments
#[derive(Clone, Debug, Parser)]
#[command(version)]
pub struct Args {
    /// Address and port of the local node's JSON-RPC endpoint.
    ///
    /// The sweep is designed for a local, fully synced node: it pipelines
    /// requests at `--concurrency`, which is impolite to remote public nodes.
    #[arg(long, default_value = "127.0.0.1:8232")]
    pub rpc_addr: SocketAddr,

    /// Network to assemble the known-hash list for: `mainnet` or `testnet`.
    #[arg(long, default_value = "mainnet")]
    pub network: Network,

    /// First height to sweep.
    ///
    /// Sweeps are resumable: heights already present in both sweep files are
    /// always skipped, so this is only needed to restrict the swept range.
    #[arg(long, default_value = "0")]
    pub start_height: Height,

    /// Last height to sweep or emit, inclusive.
    ///
    /// During sweeps this defaults to the node tip minus a confirmation margin
    /// of 100 blocks. It is required by `--emit-chunks`, where it becomes the
    /// `max_height` of the emitted list, and must be a checkpoint height.
    #[arg(long)]
    pub end_height: Option<Height>,

    /// Directory for the sweep files and the emitted chunk files.
    #[arg(long)]
    pub out_dir: PathBuf,

    /// Per-height block size file: a `u32` little-endian value at offset
    /// `height * 4`, where `0` means "not fetched yet".
    ///
    /// Defaults to `<out-dir>/block-sizes-<network>.u32le`.
    #[arg(long)]
    pub sizes_file: Option<PathBuf>,

    /// Number of concurrent RPC requests during the sweep.
    #[arg(long, default_value = "32")]
    pub concurrency: usize,

    /// Convert a completed sweep into chunk files, verify them, and print the
    /// `KnownHashListSpec` constant, instead of sweeping.
    ///
    /// Requires `--end-height`, and a sweep that covers every height up to it.
    #[arg(long)]
    pub emit_chunks: bool,
}
