//! Calculation of Block Subsidy, Funding Streams, and Founders’ Reward

use super::*;

use zebra_chain::{
    block::Height,
    parameters::{Network, NetworkUpgrade::*},
};

use crate::parameters::{fs, Params};

fn slow_start_shift() -> Height {
    Height(Params::SLOW_START_INTERVAL / 2)
}

fn slow_start_rate() -> u32 {
    Params::MAX_BLOCK_SUBSIDY / Params::SLOW_START_INTERVAL
}

fn is_bolossom_activated(height: block::Height, network: Network) -> Option<Height> {
    let blossom_height = Blossom.activation_height(network)?;
    let condition = height >= blossom_height;
    match condition {
        true => Some(blossom_height),
        false => None,
    }
}

fn halving(height: Height, network: Network) -> u32 {
    let blossom_height = is_bolossom_activated(height, network);
    let condition = height < slow_start_shift();
    match condition {
        true => 0,
        false => match blossom_height {
            Some(blossom_height) => {
                let scaled_halvings = ((blossom_height.0 - slow_start_shift().0)
                    * Params::BLOSSOM_POW_TARGET_SPACING_RATIO)
                    + (height.0 - blossom_height.0);
                scaled_halvings / Params::POST_BLOSSOM_HALVING_INTERVAL
            }
            _ => {
                (((height.0 - slow_start_shift().0) / Params::PRE_BLOSSOM_HALVING_INTERVAL) as f32)
                    .floor() as u32
            }
        },
    }
}

pub fn block_subsidy(height: Height, network: Network) -> u32 {
    if height < slow_start_shift() {
        slow_start_rate() * height.0
    } else if slow_start_shift() <= height && height < Height(Params::SLOW_START_INTERVAL) {
        slow_start_rate() * (height.0 + 1)
    } else {
        let blossom_height = is_bolossom_activated(height, network);
        let condition = blossom_height.is_none() && Params::SLOW_START_INTERVAL <= height.0;
        match condition {
            true => Params::MAX_BLOCK_SUBSIDY >> halving(height, network),
            false => ((Params::MAX_BLOCK_SUBSIDY / Params::BLOSSOM_POW_TARGET_SPACING_RATIO
                * 2u32.pow(halving(height, network))) as f32)
                .floor() as u32,
        }
    }
}

fn founders_reward(height: Height, network: Network) -> u32 {
    let condition = halving(height, network) < 1;
    match condition {
        true => (block_subsidy(height, network) as f32 * Params::FOUNDERS_FRACTION).floor() as u32,
        false => 0,
    }
}

fn funding_stream(height: Height, network: Network, receiver: fs::Receiver) -> u32 {
    let condition = height.0 >= Params::CANOPY_ACTIVATION_HEIGHT
        && fs::mainnet::START_HEIGHT <= height.0
        && height.0 < fs::mainnet::END_HEIGHT;
    match condition {
        true => (block_subsidy(height, network) as f32
            * (fs::mainnet::numerator(receiver) as f32 / fs::mainnet::DENOMINATOR as f32))
            .floor() as u32,
        false => 0,
    }
}

pub fn miner_subsidy(height: Height, network: Network) -> u32 {
    let mut funding_streams: u32 = 0;
    funding_streams += funding_stream(height, network, fs::Receiver::ECC);
    funding_streams += funding_stream(height, network, fs::Receiver::ZF);
    funding_streams += funding_stream(height, network, fs::Receiver::MG);

    block_subsidy(height, network) - founders_reward(height, network) - funding_streams
}

#[test]
fn test_halving() -> Result<(), Report> {
    assert_eq!(
        0,
        halving(Height(Params::LAST_FOUNDER_REWARD_HEIGHT), Network::Mainnet)
    );
    assert_eq!(
        1,
        halving(
            Height(Params::LAST_FOUNDER_REWARD_HEIGHT + 1),
            Network::Mainnet
        )
    );

    Ok(())
}

#[test]
fn test_block_subsidy() -> Result<(), Report> {
    let mut total_subsidy: u64 = 0;
    for n_height in 1..Params::CANOPY_ACTIVATION_HEIGHT {
        let subsidy = (block_subsidy(Height(n_height), Network::Mainnet) / 5) as u64;
        total_subsidy += subsidy;
    }
    assert!(total_subsidy == Params::MAX_MONEY / 10);

    Ok(())
}

#[test]
fn test_founders_reward() -> Result<(), Report> {
    assert_eq!(0, founders_reward(Height(0), Network::Mainnet));
    assert_eq!(12500, founders_reward(Height(1), Network::Mainnet));
    assert_eq!(
        125000000,
        founders_reward(Height(Params::LAST_FOUNDER_REWARD_HEIGHT), Network::Mainnet)
    );
    assert_eq!(
        0,
        founders_reward(
            Height(Params::LAST_FOUNDER_REWARD_HEIGHT + 1),
            Network::Mainnet
        )
    );

    Ok(())
}

#[test]
fn test_funding_stream() -> Result<(), Report> {
    assert_eq!(
        0,
        funding_stream(Height(0), Network::Mainnet, fs::Receiver::ECC)
    );
    assert_eq!(
        0,
        funding_stream(Height(0), Network::Mainnet, fs::Receiver::ZF)
    );
    assert_eq!(
        0,
        funding_stream(Height(0), Network::Mainnet, fs::Receiver::MG)
    );

    assert_eq!(
        0,
        funding_stream(Height(1), Network::Mainnet, fs::Receiver::ECC)
    );
    assert_eq!(
        0,
        funding_stream(Height(1), Network::Mainnet, fs::Receiver::ZF)
    );
    assert_eq!(
        0,
        funding_stream(Height(1), Network::Mainnet, fs::Receiver::MG)
    );

    assert_eq!(
        0,
        funding_stream(
            Height(Params::LAST_FOUNDER_REWARD_HEIGHT),
            Network::Mainnet,
            fs::Receiver::ECC
        )
    );
    assert_eq!(
        0,
        funding_stream(
            Height(Params::LAST_FOUNDER_REWARD_HEIGHT),
            Network::Mainnet,
            fs::Receiver::ZF
        )
    );
    assert_eq!(
        0,
        funding_stream(
            Height(Params::LAST_FOUNDER_REWARD_HEIGHT),
            Network::Mainnet,
            fs::Receiver::MG
        )
    );

    assert_eq!(
        87500000,
        funding_stream(
            Height(Params::LAST_FOUNDER_REWARD_HEIGHT + 1),
            Network::Mainnet,
            fs::Receiver::ECC
        )
    );
    assert_eq!(
        62500000,
        funding_stream(
            Height(Params::LAST_FOUNDER_REWARD_HEIGHT + 1),
            Network::Mainnet,
            fs::Receiver::ZF
        )
    );
    assert_eq!(
        100000000,
        funding_stream(
            Height(Params::LAST_FOUNDER_REWARD_HEIGHT + 1),
            Network::Mainnet,
            fs::Receiver::MG
        )
    );

    Ok(())
}

#[test]
fn miner_subsidy_test() -> Result<(), Report> {
    assert_eq!(0, miner_subsidy(Height(0), Network::Mainnet));
    assert_eq!(50000, miner_subsidy(Height(1), Network::Mainnet));
    assert_eq!(
        500000000,
        miner_subsidy(Height(Params::LAST_FOUNDER_REWARD_HEIGHT), Network::Mainnet)
    );
    assert_eq!(
        1000000000,
        miner_subsidy(
            Height(Params::LAST_FOUNDER_REWARD_HEIGHT + 1),
            Network::Mainnet
        )
    );

    Ok(())
}
