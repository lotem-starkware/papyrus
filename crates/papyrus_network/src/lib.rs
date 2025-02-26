/// This crate is responsible for sending messages to a given peer and responding to them according
/// to the [`Starknet p2p specs`]
///
/// [`Starknet p2p specs`]: https://github.com/starknet-io/starknet-p2p-specs/
pub mod messages;
pub mod streamed_data;
#[cfg(test)]
mod test_utils;

use starknet_api::block::{BlockHash, BlockNumber};

pub enum Direction {
    Forward,
    Backward,
}

pub enum BlockID {
    Hash(BlockHash),
    Number(BlockNumber),
}

pub struct BlockQuery {
    pub start: BlockID,
    pub direction: Direction,
    pub limit: u64,
    pub skip: u64,
    pub step: u64,
}

// TODO(shahak): Implement conversion from GetBlocks to BlockQuery.
