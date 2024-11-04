//!
//!

impl From<crate::methods::GetBlockChainInfo> for crate::server::GetBlockChainInfo {
    fn from(info: crate::methods::GetBlockChainInfo) -> Self {
        // Convert value_pools array to Vec for Protobuf
        let value_pools: Vec<crate::server::ValuePoolBalance> = info
            .value_pools
            .iter()
            .map(|pool| crate::server::ValuePoolBalance {
                id: pool.data().0.to_string(),
                chain_value: pool.data().1.lossy_zec(),
                chain_value_zat: pool.data().2.into(),
            })
            .collect();

        // Convert `upgrades` to a list of ordered entries
        let upgrades: Vec<crate::server::UpgradeEntry> = info
            .upgrades
            .into_iter()
            .map(|(key, upgrade_info)| crate::server::UpgradeEntry {
                key: key.0.to_string(),
                value: Some(crate::server::NetworkUpgradeInfo {
                    name: upgrade_info.name.to_string(),
                    status: upgrade_info.status.to_string(),
                    activation_height: upgrade_info.activation_height.0,
                }),
            })
            .collect();

        // Convert consensus branch
        let consensus = crate::server::TipConsensusBranch {
            chain_tip: info.consensus.chain_tip.0.to_string(),
            next_block: info.consensus.next_block.0.to_string(),
        };

        crate::server::GetBlockChainInfo {
            chain: info.chain,
            blocks: info.blocks.0, // Assuming `Height` wraps an integer
            best_block_hash: hex::encode(info.best_block_hash.0), // Encode to hex if necessary
            estimated_height: info.estimated_height.0,
            value_pools,
            upgrades,
            consensus: Some(consensus),
        }
    }
}

impl From<crate::server::AddressStrings> for crate::methods::AddressStrings {
    fn from(addresses: crate::server::AddressStrings) -> Self {
        crate::methods::AddressStrings {
            addresses: addresses.addresses,
        }
    }
}

impl From<crate::methods::AddressBalance> for crate::server::AddressBalance {
    fn from(balance: crate::methods::AddressBalance) -> Self {
        crate::server::AddressBalance {
            balance: balance.balance,
        }
    }
}

///
pub struct RawTransactionHex(pub String);

impl From<crate::methods::SentTransactionHash> for crate::server::SentTransactionHash {
    fn from(hash: crate::methods::SentTransactionHash) -> Self {
        crate::server::SentTransactionHash {
            hash: hash.0.to_string(),
        }
    }
}
