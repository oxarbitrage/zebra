//! Zebra supported RPC methods.
//!
//! Based on the [`zcashd` RPC methods](https://zcash.github.io/rpc/)
//! as used by `lightwalletd.`
//!
//! Some parts of the `zcashd` RPC documentation are outdated.
//! So this implementation follows the `lightwalletd` client implementation.

use futures::FutureExt;
use jsonrpc_core::{self, BoxFuture, Result};
use jsonrpc_derive::rpc;

use tower::{buffer::Buffer, util::BoxService, ServiceExt};

use zebra_chain::block::Height;
use zebra_network::constants::USER_AGENT;

type State = Buffer<
    BoxService<zebra_state::Request, zebra_state::Response, zebra_state::BoxError>,
    zebra_state::Request,
>;

#[cfg(test)]
mod tests;

#[rpc(server)]
/// RPC method signatures.
pub trait Rpc {
    /// getinfo
    ///
    /// Returns software information from the RPC server running Zebra.
    ///
    /// zcashd reference: <https://zcash.github.io/rpc/getinfo.html>
    ///
    /// Result:
    /// {
    ///      "build": String, // Full application version
    ///      "subversion", String, // Zebra user agent
    /// }
    ///
    /// Note 1: We only expose 2 fields as they are the only ones needed for
    /// lightwalletd: <https://github.com/zcash/lightwalletd/blob/v0.4.9/common/common.go#L91-L95>
    ///
    /// Note 2: <https://zcash.github.io/rpc/getinfo.html> is outdated so it does not
    /// show the fields we are exposing. However, this fields are part of the output
    /// as shown in the following zcashd code:
    /// <https://github.com/zcash/zcash/blob/v4.6.0-1/src/rpc/misc.cpp#L86-L87>
    /// Zcash open ticket to add this fields to the docs: <https://github.com/zcash/zcash/issues/5606>
    #[rpc(name = "getinfo")]
    fn get_info(&self) -> Result<GetInfo>;

    /// getblockchaininfo
    ///
    /// TODO: explain what the method does
    ///       link to the zcashd RPC reference
    ///       list the arguments and fields that lightwalletd uses
    ///       note any other lightwalletd changes
    #[rpc(name = "getblockchaininfo")]
    fn get_blockchain_info(&self) -> Result<GetBlockChainInfo>;

    /// getblock
    ///
    /// Returns ...
    ///
    /// zcashd reference: <https://zcash.github.io/rpc/getblock.html>
    ///
    /// Result:
    /// {
    ///      "data": String, // Add comment
    /// }
    #[rpc(name = "getblock")]
    fn get_block(&self, height: Height) -> BoxFuture<Result<GetBlock>>;
}

/// RPC method implementations.

pub struct RpcImpl {
    /// Zebra's application version.
    pub app_version: String,
    pub state_service: State,
}
impl Rpc for RpcImpl {
    fn get_info(&self) -> Result<GetInfo> {
        let response = GetInfo {
            build: self.app_version.clone(),
            subversion: USER_AGENT.into(),
        };

        Ok(response)
    }

    fn get_blockchain_info(&self) -> Result<GetBlockChainInfo> {
        // TODO: dummy output data, fix in the context of #3143
        let response = GetBlockChainInfo {
            chain: "TODO: main".to_string(),
        };

        Ok(response)
    }

    fn get_block(&self, height: Height) -> BoxFuture<Result<GetBlock>> {
        let state = self.state_service.clone();

        async move {
            let res = state
                .oneshot(zebra_state::Request::Block(
                    zebra_state::HashOrHeight::Height(height),
                ))
                .await
                .map_err(|error| error.to_string());

            match res.unwrap() {
                zebra_state::Response::Block(Some(block)) => Ok(GetBlock {
                    data: block.to_string(),
                }),
                _ => unreachable!("whatever"),
            }
        }
        .boxed()
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
/// Response to a `getinfo` RPC request.
pub struct GetInfo {
    build: String,
    subversion: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
/// Response to a `getblockchaininfo` RPC request.
pub struct GetBlockChainInfo {
    chain: String,
    // TODO: add other fields used by lightwalletd (#3143)
}

#[derive(serde::Serialize, serde::Deserialize)]
/// Response to a `getblock` RPC request.
pub struct GetBlock {
    data: String,
}
