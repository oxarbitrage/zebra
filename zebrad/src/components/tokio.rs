//! A component owning the Tokio runtime.

use crate::prelude::*;
use abscissa_core::{Application, Component, FrameworkError, Shutdown};
use color_eyre::Report;
use std::{future::Future, time::Duration};
use tokio::runtime::Runtime;

/// When Zebra is shutting down, wait this long for tokio tasks to finish.
const TOKIO_SHUTDOWN_TIMEOUT: Duration = std::time::Duration::from_secs(20);

/// An Abscissa component which owns a Tokio runtime.
///
/// The runtime is stored as an `Option` so that when it's time to enter an async
/// context by calling `block_on` with a "root future", the runtime can be taken
/// independently of Abscissa's component locking system. Otherwise whatever
/// calls `block_on` holds an application lock for the entire lifetime of the
/// async context.
#[derive(Component, Debug)]
pub struct TokioComponent {
    pub rt: Option<Runtime>,
}

impl TokioComponent {
    pub fn new() -> Result<Self, FrameworkError> {
        Ok(Self {
            rt: Some(
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .unwrap(),
            ),
        })
    }
}

/// Zebrad's graceful shutdown function, blocks until one of the supported
/// shutdown signals is received.
async fn shutdown() {
    imp::shutdown().await;
}

/// Extension trait to centralize entry point for runnable subcommands that
/// depend on tokio
pub(crate) trait RuntimeRun {
    fn run(self, fut: impl Future<Output = Result<(), Report>>);
}

impl RuntimeRun for Runtime {
    fn run(self, fut: impl Future<Output = Result<(), Report>>) {
        let result = self.block_on(async move {
            // Always poll the shutdown future first.
            //
            // Otherwise, a busy Zebra instance could starve the shutdown future,
            // and delay shutting down.
            tokio::select! {
                biased;
                _ = shutdown() => Ok(()),
                result = fut => result,
            }
        });

        // Don't wait for long blocking tasks before shutting down
        tracing::info!(
            ?TOKIO_SHUTDOWN_TIMEOUT,
            "waiting for async tokio tasks to shut down"
        );
        self.shutdown_timeout(TOKIO_SHUTDOWN_TIMEOUT);

        match result {
            Ok(()) => {
                info!("shutting down Zebra");
            }
            Err(error) => {
                warn!(?error, "shutting down Zebra due to an error");
                app_writer().shutdown(Shutdown::Forced);
            }
        }
    }
}

#[cfg(unix)]
mod imp {
    use tokio::signal::unix::{signal, SignalKind};
    use tracing::info;

    pub(super) async fn shutdown() {
        // If both signals are received, select! chooses one of them at random.
        tokio::select! {
            // SIGINT  - Terminal interrupt signal. Typically generated by shells in response to Ctrl-C.
            _ = sig(SignalKind::interrupt(), "SIGINT") => {}
            // SIGTERM - Standard shutdown signal used by process launchers.
            _ = sig(SignalKind::terminate(), "SIGTERM") => {}
        };
    }

    #[instrument]
    async fn sig(kind: SignalKind, name: &'static str) {
        // Create a Future that completes the first
        // time the process receives 'sig'.
        signal(kind)
            .expect("Failed to register signal handler")
            .recv()
            .await;
        zebra_chain::shutdown::set_shutting_down();

        info!(
            // use target to remove 'imp' from output
            target: "zebrad::signal",
            "received {}, starting shutdown",
            name,
        );
    }
}

#[cfg(not(unix))]
mod imp {
    use tracing::info;

    pub(super) async fn shutdown() {
        //  Wait for Ctrl-C in Windows terminals.
        // (Zebra doesn't support NT Service control messages. Use a service wrapper for long-running instances.)
        tokio::signal::ctrl_c()
            .await
            .expect("listening for ctrl-c signal should never fail");
        zebra_chain::shutdown::set_shutting_down();

        info!(
            // use target to remove 'imp' from output
            target: "zebrad::signal",
            "received Ctrl-C, starting shutdown",
        );
    }
}
