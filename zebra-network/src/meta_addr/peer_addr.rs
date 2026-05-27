//! Wrappers for peer addresses which hide sensitive user and node operator details in logs and
//! metrics.

use std::{
    collections::hash_map::RandomState,
    fmt,
    hash::BuildHasher,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    ops::{Deref, DerefMut},
    str::FromStr,
    sync::OnceLock,
};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

/// A thin wrapper for [`SocketAddr`] which hides peer IP addresses in logs and metrics.
#[derive(
    Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
#[derive(schemars::JsonSchema)]
pub struct PeerSocketAddr(SocketAddr);

static PEER_TELEMETRY_HASHER: OnceLock<RandomState> = OnceLock::new();

impl fmt::Debug for PeerSocketAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for PeerSocketAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ip_version = if self.is_ipv4() { "v4" } else { "v6" };

        // The port is usually not sensitive, and it's useful for debugging.
        f.pad(&format!("{}redacted:{}", ip_version, self.port()))
    }
}

impl FromStr for PeerSocketAddr {
    type Err = <SocketAddr as FromStr>::Err;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.parse()?))
    }
}

impl<S> From<S> for PeerSocketAddr
where
    S: Into<SocketAddr>,
{
    fn from(value: S) -> Self {
        Self(value.into())
    }
}

impl Deref for PeerSocketAddr {
    type Target = SocketAddr;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for PeerSocketAddr {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl PeerSocketAddr {
    /// Returns an unspecified `PeerSocketAddr`, which can't be used for outbound connections.
    pub fn unspecified() -> Self {
        Self(SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0))
    }

    /// Return the underlying [`SocketAddr`], which allows sensitive peer address information to
    /// be printed and logged.
    pub fn remove_socket_addr_privacy(&self) -> SocketAddr {
        **self
    }

    /// Returns a per-process label for correlating peer telemetry without exposing peer IPs.
    ///
    /// The label is stable only while this process is running.
    pub fn telemetry_label(&self) -> String {
        let hasher = PEER_TELEMETRY_HASHER.get_or_init(RandomState::new);

        format!("peer_{:016x}", hasher.hash_one(self.0))
    }

    /// Returns true if the inner [`SocketAddr`]'s IP is the localhost IP.
    pub fn is_localhost(&self) -> bool {
        let ip = self.0.ip();
        ip == Ipv4Addr::LOCALHOST || ip == Ipv6Addr::LOCALHOST
    }
}

#[cfg(test)]
mod tests {
    use super::PeerSocketAddr;

    #[test]
    fn telemetry_label_is_stable_for_same_peer() {
        let peer = PeerSocketAddr::from(([192, 0, 2, 1], 8233));

        assert_eq!(peer.telemetry_label(), peer.telemetry_label());
    }

    #[test]
    fn telemetry_label_hides_peer_ip() {
        let peer = PeerSocketAddr::from(([192, 0, 2, 1], 8233));
        let label = peer.telemetry_label();

        assert!(label.starts_with("peer_"));
        assert!(!label.contains("192.0.2.1"));
        assert!(!label.contains("8233"));
    }
}
