//! Information about the current state of the best block chain.

use std::sync::Arc;

use zebra_chain::{
    block::{Header, Height},
    history_tree::HistoryTree,
    parameters::POW_AVERAGING_WINDOW,
};

use crate::{
    request::HashOrHeight,
    service::{
        check::difficulty::POW_MEDIAN_BLOCK_SPAN,
        finalized_state::ZebraDb,
        non_finalized_state::Chain,
    },
};

/// Returns the [`history_tree::HistoryTree`] ...
pub fn history_tree_root<C>(
    chain: Option<C>,
    db: &ZebraDb,
) -> Option<Arc<HistoryTree>>
where
    C: AsRef<Chain>,
{

    chain
        .as_ref()
        .and_then(|chain| Some(chain.as_ref().history_tree.clone()))
        .or_else(|| Some(db.history_tree()))
}

/// 
pub fn last_n_block_headers<C>(
    chain: Option<C>,
    db: &ZebraDb,
    height : Height,
) -> Option<Vec<Arc<Header>>>
where
    C: AsRef<Chain>,
{
    let mut headers = vec![];

    const NEEDED_CONTEXT_BLOCKS : usize = POW_AVERAGING_WINDOW + POW_MEDIAN_BLOCK_SPAN;

    if height.0 > NEEDED_CONTEXT_BLOCKS.try_into().expect("number is small enough to always fit") {

        for i in 0..(NEEDED_CONTEXT_BLOCKS) {

            let i_ready : i32 = i.try_into().expect("should always convert");

            let res = chain
                .as_ref()
                .and_then(|chain| Some(chain.as_ref().non_finalized_nth_header(i)))
                .or_else(|| Some(db.block_header(HashOrHeight::Height((height - i_ready).expect("can't fail as we are in a height greater than NEEDED_CONTEXT_BLOCKS")))));

            match res {
                Some(Some(header)) => headers.push(header.clone()),
                _ => (),
            }
        }

        Some(headers)
    }
    else {
        None
    }
}
