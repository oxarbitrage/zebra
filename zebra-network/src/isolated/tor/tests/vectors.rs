//! Fixed test vectors for isolated Zebra connections.

use std::time::Duration;

use crate::Config;

use super::super::*;

use futures::stream::FuturesUnordered;
use tokio_stream::StreamExt;
use Network::*;

/// The maximum allowed test runtime.
const MAX_TEST_DURATION: Duration = Duration::from_secs(20);

/// Test that `connect_isolated` doesn't panic when used over Tor.
///
/// (We can't connect to ourselves over Tor, so there's not much more we can do here.)
#[tokio::test]
async fn connect_isolated_run_tor_once() {
    let _init_guard = zebra_test::init();

    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    // These tests might take a long time on machines where Tor is censored.

    // Pick a mainnet seeder hostname, it doesn't matter which one.
    let config = Config::default();
    let seeder_hostname = config
        .initial_peer_hostnames()
        .iter()
        .next()
        .unwrap()
        .clone();

    connect_isolated_run_tor_once_with(Mainnet, seeder_hostname).await;
}

/// Test that `connect_isolated` can use multiple isolated Tor connections at the same time.
///
/// Use the multi-threaded runtime to test concurrent Tor instances.
#[tokio::test(flavor = "multi_thread")]
async fn connect_isolated_run_tor_multi() {
    let _init_guard = zebra_test::init();

    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    // These tests might take a long time on machines where Tor is censored.

    let mut isolated_conns = FuturesUnordered::new();

    // Use all the seeder hostnames for each network
    for network in [Mainnet, Network::new_default_testnet()] {
        let config = Config {
            network: network.clone(),
            ..Config::default()
        };

        for seeder_hostname in config.initial_peer_hostnames().iter().cloned() {
            let conn = connect_isolated_run_tor_once_with(network.clone(), seeder_hostname);
            isolated_conns.push(conn);
        }
    }

    // Wait for all the connections to complete (or timeout)
    while let Some(()) = isolated_conns.next().await {}
}

async fn connect_isolated_run_tor_once_with(network: Network, hostname: String) {
    // Connection errors are detected and ignored using the JoinHandle.
    // (They might also make the test hang.)
    let mut outbound_join_handle =
        tokio::spawn(connect_isolated_tor(network, hostname, "".to_string()));

    // Let the spawned task run for a long time.
    let outbound_join_handle_timeout =
        tokio::time::timeout(MAX_TEST_DURATION, &mut outbound_join_handle);

    // Make sure that the isolated connection did not panic.
    //
    // We can't control network reliability in the test, so the only bad outcome is a panic.
    // We make the test pass if there are network errors, if we get a valid running service,
    // or if we are still waiting for Tor or the handshake.
    let outbound_result = outbound_join_handle_timeout.await;
    assert!(matches!(outbound_result, Ok(Ok(_)) | Err(_)));

    outbound_join_handle.abort();
}

/// Throwaway probe (tor-arti-check): does a Zebra handshake over Tor actually complete?
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn tor_probe_handshake() {
    let _init_guard = zebra_test::init();
    let config = Config::default();
    for hostname in config.initial_peer_hostnames().iter().cloned() {
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(180),
            connect_isolated_tor(Mainnet, hostname.clone(), "".to_string()),
        )
        .await;
        let outcome = match result {
            Ok(Ok(_client)) => "HANDSHAKE OK".to_string(),
            Ok(Err(e)) => format!("ERROR: {e}"),
            Err(_) => "TIMEOUT after 180s".to_string(),
        };
        println!("TOR-PROBE {hostname}: {outcome} ({:.1}s)", started.elapsed().as_secs_f64());
        if outcome == "HANDSHAKE OK" {
            break;
        }
    }
}
