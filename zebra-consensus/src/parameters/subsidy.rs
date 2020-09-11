//! Constants for Block Subsidy, Funding Streams, and Founders’ Reward

use zebra_chain::parameters::Network;

/// Whatever
pub struct Params(pub Network);

impl Params {
    /// COIN
    pub const COIN: u32 = 100000000;

    /// SlowStartInterval
    pub const SLOW_START_INTERVAL: u32 = 20000;

    /// MaxBlockSubsidy
    pub const MAX_BLOCK_SUBSIDY: u32 = (12.5 * (Self::COIN as f32)) as u32;

    /// PreBlossomPoWTargetSpacing
    pub const PRE_BLOSSOM_POW_TARGET_SPACING: u32 = 150;

    /// PostBlossomPoWTargetSpacing
    pub const POST_BLOSSOM_POW_TARGET_SPACING: u32 = 75;

    /// BLOSSOM_POW_TARGET_SPACING_RATIO
    pub const BLOSSOM_POW_TARGET_SPACING_RATIO: u32 =
        Self::PRE_BLOSSOM_POW_TARGET_SPACING / Self::POST_BLOSSOM_POW_TARGET_SPACING;

    /// PreBlossomHalvingInterval
    pub const PRE_BLOSSOM_HALVING_INTERVAL: u32 = 840000;

    /// POST_BLOSSOM_HALVING_INTERVAL
    pub const POST_BLOSSOM_HALVING_INTERVAL: u32 =
        Self::PRE_BLOSSOM_HALVING_INTERVAL * Self::BLOSSOM_POW_TARGET_SPACING_RATIO;

    /// MAX_MONEY
    pub const MAX_MONEY: u64 = 21000000 * Self::COIN as u64;

    /// FoundersFraction
    pub const FOUNDERS_FRACTION: f32 = 0.2;

    /// CanopyActivationHeight
    pub const CANOPY_ACTIVATION_HEIGHT: u32 = 1046400; // mainnet

    /// GetLastFoundersRewardHeight
    pub const LAST_FOUNDER_REWARD_HEIGHT: u32 = Self::CANOPY_ACTIVATION_HEIGHT - 1;
}

/// Funding Streams
// Todo: There is probably a better way to do this ...
pub mod fs {

    /// The funding stream receivers
    pub enum Receiver {
        /// Electric Coin Company
        ECC,
        /// ZCash Foundation
        ZF,
        /// Major Grants
        MG,
    }

    /// For the Mainnet
    pub mod mainnet {
        /// Denominator
        pub const DENOMINATOR: u32 = 100;
        /// Start height
        pub const START_HEIGHT: u32 = 1046400;
        /// End height
        pub const END_HEIGHT: u32 = 2726400;

        use super::Receiver;
        /// Numerator based on receiver
        pub fn numerator(receiver: Receiver) -> u32 {
            match receiver {
                Receiver::ECC => 7,
                Receiver::ZF => 5,
                Receiver::MG => 8,
            }
        }
    }
    /// For the Testnet
    pub mod testnet {
        /// Denominator
        pub const DENOMINATOR: u32 = 100;
        /// Start height
        pub const START_HEIGHT: u32 = 1028500;
        /// End height
        pub const END_HEIGHT: u32 = 2796000;

        use super::Receiver;
        /// Numerator based on receiver
        pub fn numerator(receiver: Receiver) -> u32 {
            match receiver {
                Receiver::ECC => 7,
                Receiver::ZF => 5,
                Receiver::MG => 8,
            }
        }
    }
}
