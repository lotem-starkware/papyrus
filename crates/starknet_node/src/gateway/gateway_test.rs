use assert_matches::assert_matches;
use jsonrpsee::core::Error;
use jsonrpsee::http_client::HttpClientBuilder;
use jsonrpsee::http_server::types::error::CallError;
use jsonrpsee::types::error::ErrorObject;
use jsonrpsee::types::EmptyParams;
use starknet_api::{
    shash, BlockBody, BlockHash, BlockHeader, BlockNumber, CallData, ClassHash, ContractAddress,
    ContractAddressSalt, ContractClass, DeclareTransactionReceipt, DeployTransaction,
    DeployedContract, GlobalRoot, StarkFelt, StarkHash, StateDiffForward, StorageDiff,
    StorageEntry, StorageKey, Transaction, TransactionHash, TransactionReceipt, TransactionVersion,
};

use super::api::{
    BlockHashAndNumber, BlockHashOrNumber, BlockId, JsonRpcClient, JsonRpcError, JsonRpcServer, Tag,
};
use super::objects::{
    from_starknet_storage_diffs, Block, StateDiff, StateUpdate, TransactionWithType, Transactions,
};
use super::{run_server, GatewayConfig, JsonRpcServerImpl};
use crate::storage::{test_utils, BodyStorageWriter, HeaderStorageWriter, StateStorageWriter};

// TODO(anatg): Move out of the gateway so that storage and sync can use it too.
fn get_test_block(transaction_count: usize) -> (BlockHeader, BlockBody) {
    let header = BlockHeader {
        block_hash: BlockHash(shash!(
            "0x642b629ad8ce233b55798c83bb629a59bf0a0092f67da28d6d66776680d5483"
        )),
        block_number: BlockNumber(0),
        ..BlockHeader::default()
    };
    let mut transactions = vec![];
    for i in 0..transaction_count {
        let transaction = Transaction::Deploy(DeployTransaction {
            transaction_hash: TransactionHash(StarkHash::from_u64(i as u64)),
            version: TransactionVersion(shash!("0x1")),
            contract_address: ContractAddress(shash!("0x2")),
            constructor_calldata: CallData(vec![shash!("0x3")]),
            class_hash: ClassHash(StarkHash::from_u64(i as u64)),
            contract_address_salt: ContractAddressSalt(shash!("0x4")),
        });
        transactions.push(transaction);
    }
    let body = BlockBody { transactions };
    (header, body)
}

fn get_test_state_diff() -> (BlockHeader, BlockHeader, StateDiffForward) {
    let parent_hash =
        BlockHash(shash!("0x642b629ad8ce233b55798c83bb629a59bf0a0092f67da28d6d66776680d5483"));
    let state_root = GlobalRoot(shash!("0x12"));
    let parent_header = BlockHeader {
        block_number: BlockNumber(0),
        block_hash: parent_hash,
        state_root,
        ..BlockHeader::default()
    };

    let block_hash =
        BlockHash(shash!("0x642b629ad8ce233b55798c83bb629a59bf0a0092f67da28d6d66776680d5493"));
    let header = BlockHeader {
        block_number: BlockNumber(1),
        block_hash,
        parent_hash,
        ..BlockHeader::default()
    };

    let address = ContractAddress(shash!("0x11"));
    let class_hash = ClassHash(shash!("0x4"));
    let address_2 = ContractAddress(shash!("0x21"));
    let class_hash_2 = ClassHash(shash!("0x5"));
    let key = StorageKey(shash!("0x1001"));
    let value = shash!("0x200");
    let key_2 = StorageKey(shash!("0x1002"));
    let value_2 = shash!("0x201");
    let diff = StateDiffForward {
        deployed_contracts: vec![
            DeployedContract { address, class_hash },
            DeployedContract { address: address_2, class_hash: class_hash_2 },
        ],
        storage_diffs: vec![
            StorageDiff {
                address,
                diff: vec![
                    StorageEntry { key: key.clone(), value },
                    StorageEntry { key: key_2, value: value_2 },
                ],
            },
            StorageDiff { address: address_2, diff: vec![StorageEntry { key, value }] },
        ],
    };

    (parent_header, header, diff)
}

#[tokio::test]
async fn test_block_number() -> Result<(), anyhow::Error> {
    let (storage_reader, mut storage_writer) = test_utils::get_test_storage();
    let module = JsonRpcServerImpl { storage_reader }.into_rpc();

    // No blocks yet.
    let err = module
        .call::<_, BlockNumber>("starknet_blockNumber", EmptyParams::new())
        .await
        .unwrap_err();
    assert_matches!(err, Error::Call(CallError::Custom(err)) if err == ErrorObject::owned(
        JsonRpcError::NoBlocks as i32,
        JsonRpcError::NoBlocks.to_string(),
        None::<()>,
    ));

    // Add a block and check again.
    storage_writer
        .begin_rw_txn()?
        .append_header(BlockNumber(0), &BlockHeader::default())?
        .commit()?;
    let block_number =
        module.call::<_, BlockNumber>("starknet_blockNumber", EmptyParams::new()).await?;
    assert_eq!(block_number, BlockNumber(0));
    Ok(())
}

#[tokio::test]
async fn test_block_hash_and_number() -> Result<(), anyhow::Error> {
    let (storage_reader, mut storage_writer) = test_utils::get_test_storage();
    let module = JsonRpcServerImpl { storage_reader }.into_rpc();

    // No blocks yet.
    let err = module
        .call::<_, BlockHashAndNumber>("starknet_blockHashAndNumber", EmptyParams::new())
        .await
        .unwrap_err();
    assert_matches!(err, Error::Call(CallError::Custom(err)) if err == ErrorObject::owned(
        JsonRpcError::NoBlocks as i32,
        JsonRpcError::NoBlocks.to_string(),
        None::<()>,
    ));

    // Add a block and check again.
    let (header, _) = get_test_block(1);
    storage_writer.begin_rw_txn()?.append_header(header.block_number, &header)?.commit()?;
    let block_hash_and_number = module
        .call::<_, BlockHashAndNumber>("starknet_blockHashAndNumber", EmptyParams::new())
        .await?;
    assert_eq!(
        block_hash_and_number,
        BlockHashAndNumber { block_hash: header.block_hash, block_number: header.block_number }
    );
    Ok(())
}

#[tokio::test]
async fn test_get_block_w_transaction_hashes() -> Result<(), anyhow::Error> {
    let (storage_reader, mut storage_writer) = test_utils::get_test_storage();
    let module = JsonRpcServerImpl { storage_reader }.into_rpc();

    let (header, body) = get_test_block(1);
    storage_writer
        .begin_rw_txn()?
        .append_header(header.block_number, &header)?
        .append_body(header.block_number, &body)?
        .commit()?;

    let expected_transaction = body.transactions.get(0).unwrap();
    let expected_block = Block {
        header: header.into(),
        transactions: Transactions::Hashes(vec![expected_transaction.transaction_hash()]),
    };

    // Get block by hash.
    let block = module
        .call::<_, Block>(
            "starknet_getBlockWithTxHashes",
            [BlockId::HashOrNumber(BlockHashOrNumber::Hash(expected_block.header.block_hash))],
        )
        .await
        .unwrap();
    assert_eq!(block, expected_block);

    // Get block by number.
    let block = module
        .call::<_, Block>(
            "starknet_getBlockWithTxHashes",
            [BlockId::HashOrNumber(BlockHashOrNumber::Number(expected_block.header.block_number))],
        )
        .await?;
    assert_eq!(block, expected_block);

    // Ask for the latest block.
    let block = module
        .call::<_, Block>("starknet_getBlockWithTxHashes", [BlockId::Tag(Tag::Latest)])
        .await?;
    assert_eq!(block, expected_block);

    // Ask for an invalid block hash.
    let err = module
        .call::<_, Block>(
            "starknet_getBlockWithTxHashes",
            [BlockId::HashOrNumber(BlockHashOrNumber::Hash(BlockHash(shash!(
                "0x642b629ad8ce233b55798c83bb629a59bf0a0092f67da28d6d66776680d5484"
            ))))],
        )
        .await
        .unwrap_err();
    assert_matches!(err, Error::Call(CallError::Custom(err)) if err == ErrorObject::owned(
        JsonRpcError::InvalidBlockId as i32,
        JsonRpcError::InvalidBlockId.to_string(),
        None::<()>,
    ));

    // Ask for an invalid block number.
    let err = module
        .call::<_, Block>(
            "starknet_getBlockWithTxHashes",
            [BlockId::HashOrNumber(BlockHashOrNumber::Number(BlockNumber(1)))],
        )
        .await
        .unwrap_err();
    assert_matches!(err, Error::Call(CallError::Custom(err)) if err == ErrorObject::owned(
        JsonRpcError::InvalidBlockId as i32,
        JsonRpcError::InvalidBlockId.to_string(),
        None::<()>,
    ));
    Ok(())
}

#[tokio::test]
async fn test_get_block_w_full_transactions() -> Result<(), anyhow::Error> {
    let (storage_reader, mut storage_writer) = test_utils::get_test_storage();
    let module = JsonRpcServerImpl { storage_reader }.into_rpc();

    let (header, body) = get_test_block(1);
    storage_writer
        .begin_rw_txn()?
        .append_header(header.block_number, &header)?
        .append_body(header.block_number, &body)?
        .commit()?;

    let expected_transaction = body.transactions.get(0).unwrap();
    let expected_block = Block {
        header: header.into(),
        transactions: Transactions::Full(vec![expected_transaction.clone().into()]),
    };

    // Get block by hash.
    let block = module
        .call::<_, Block>(
            "starknet_getBlockWithTxs",
            [BlockId::HashOrNumber(BlockHashOrNumber::Hash(expected_block.header.block_hash))],
        )
        .await?;
    assert_eq!(block, expected_block);

    // Get block by number.
    let block = module
        .call::<_, Block>(
            "starknet_getBlockWithTxs",
            [BlockId::HashOrNumber(BlockHashOrNumber::Number(expected_block.header.block_number))],
        )
        .await?;
    assert_eq!(block, expected_block);

    // Ask for the latest block.
    let block =
        module.call::<_, Block>("starknet_getBlockWithTxs", [BlockId::Tag(Tag::Latest)]).await?;
    assert_eq!(block, expected_block);

    // Ask for an invalid block hash.
    let err = module
        .call::<_, Block>(
            "starknet_getBlockWithTxs",
            [BlockId::HashOrNumber(BlockHashOrNumber::Hash(BlockHash(shash!(
                "0x642b629ad8ce233b55798c83bb629a59bf0a0092f67da28d6d66776680d5484"
            ))))],
        )
        .await
        .unwrap_err();
    assert_matches!(err, Error::Call(CallError::Custom(err)) if err == ErrorObject::owned(
        JsonRpcError::InvalidBlockId as i32,
        JsonRpcError::InvalidBlockId.to_string(),
        None::<()>,
    ));

    // Ask for an invalid block number.
    let err = module
        .call::<_, Block>(
            "starknet_getBlockWithTxs",
            [BlockId::HashOrNumber(BlockHashOrNumber::Number(BlockNumber(1)))],
        )
        .await
        .unwrap_err();
    assert_matches!(err, Error::Call(CallError::Custom(err)) if err == ErrorObject::owned(
        JsonRpcError::InvalidBlockId as i32,
        JsonRpcError::InvalidBlockId.to_string(),
        None::<()>,
    ));
    Ok(())
}

#[tokio::test]
async fn test_get_storage_at() -> Result<(), anyhow::Error> {
    let (storage_reader, mut storage_writer) = test_utils::get_test_storage();
    let module = JsonRpcServerImpl { storage_reader }.into_rpc();

    let (header, _, diff) = get_test_state_diff();
    storage_writer
        .begin_rw_txn()?
        .append_header(header.block_number, &header)?
        .append_state_diff(header.block_number, &diff)?
        .commit()?;

    let address = diff.storage_diffs.get(0).unwrap().address;
    let key = diff.storage_diffs.get(0).unwrap().diff.get(0).unwrap().key.clone();
    let expected_value = diff.storage_diffs.get(0).unwrap().diff.get(0).unwrap().value;

    // Get storage by block hash.
    let res = module
        .call::<_, StarkFelt>(
            "starknet_getStorageAt",
            (
                address,
                key.clone(),
                BlockId::HashOrNumber(BlockHashOrNumber::Hash(header.block_hash)),
            ),
        )
        .await?;
    assert_eq!(res, expected_value);

    // Get storage by block number.
    let res = module
        .call::<_, StarkFelt>(
            "starknet_getStorageAt",
            (
                address,
                key.clone(),
                BlockId::HashOrNumber(BlockHashOrNumber::Number(header.block_number)),
            ),
        )
        .await?;
    assert_eq!(res, expected_value);

    // Ask for an invalid contract.
    let err = module
        .call::<_, StarkFelt>(
            "starknet_getStorageAt",
            (
                ContractAddress(shash!("0x12")),
                key.clone(),
                BlockId::HashOrNumber(BlockHashOrNumber::Hash(header.block_hash)),
            ),
        )
        .await
        .unwrap_err();
    assert_matches!(err, Error::Call(CallError::Custom(err)) if err == ErrorObject::owned(
        JsonRpcError::ContractNotFound as i32,
        JsonRpcError::ContractNotFound.to_string(),
        None::<()>,
    ));

    // Ask for an invalid block hash.
    let err = module
        .call::<_, StarkFelt>(
            "starknet_getStorageAt",
            (
                address,
                key.clone(),
                BlockId::HashOrNumber(BlockHashOrNumber::Hash(BlockHash(shash!(
                    "0x642b629ad8ce233b55798c83bb629a59bf0a0092f67da28d6d66776680d5484"
                )))),
            ),
        )
        .await
        .unwrap_err();
    assert_matches!(err, Error::Call(CallError::Custom(err)) if err == ErrorObject::owned(
        JsonRpcError::InvalidBlockId as i32,
        JsonRpcError::InvalidBlockId.to_string(),
        None::<()>,
    ));

    // Ask for an invalid block number.
    let err = module
        .call::<_, StarkFelt>(
            "starknet_getStorageAt",
            (
                address,
                key.clone(),
                BlockId::HashOrNumber(BlockHashOrNumber::Number(BlockNumber(1))),
            ),
        )
        .await
        .unwrap_err();
    assert_matches!(err, Error::Call(CallError::Custom(err)) if err == ErrorObject::owned(
        JsonRpcError::InvalidBlockId as i32,
        JsonRpcError::InvalidBlockId.to_string(),
        None::<()>,
    ));

    Ok(())
}

#[tokio::test]
async fn test_get_class_hash_at() -> Result<(), anyhow::Error> {
    let (storage_reader, mut storage_writer) = test_utils::get_test_storage();
    let module = JsonRpcServerImpl { storage_reader }.into_rpc();

    let (header, _, diff) = get_test_state_diff();
    storage_writer
        .begin_rw_txn()?
        .append_header(header.block_number, &header)?
        .append_state_diff(header.block_number, &diff)?
        .commit()?;

    let address = diff.deployed_contracts.get(0).unwrap().address;
    let expected_class_hash = diff.deployed_contracts.get(0).unwrap().class_hash;

    // Get class hash by block hash.
    let res = module
        .call::<_, ClassHash>(
            "starknet_getClassHashAt",
            (BlockId::HashOrNumber(BlockHashOrNumber::Hash(header.block_hash)), address),
        )
        .await?;
    assert_eq!(res, expected_class_hash);

    // Get class hash by block number.
    let res = module
        .call::<_, ClassHash>(
            "starknet_getClassHashAt",
            (BlockId::HashOrNumber(BlockHashOrNumber::Number(header.block_number)), address),
        )
        .await?;
    assert_eq!(res, expected_class_hash);

    // Ask for an invalid contract.
    let err = module
        .call::<_, ClassHash>(
            "starknet_getClassHashAt",
            (
                BlockId::HashOrNumber(BlockHashOrNumber::Number(header.block_number)),
                ContractAddress(shash!("0x12")),
            ),
        )
        .await
        .unwrap_err();
    assert_matches!(err, Error::Call(CallError::Custom(err)) if err == ErrorObject::owned(
        JsonRpcError::ContractNotFound as i32,
        JsonRpcError::ContractNotFound.to_string(),
        None::<()>,
    ));

    // Ask for an invalid block hash.
    let err = module
        .call::<_, ClassHash>(
            "starknet_getClassHashAt",
            (
                BlockId::HashOrNumber(BlockHashOrNumber::Hash(BlockHash(shash!(
                    "0x642b629ad8ce233b55798c83bb629a59bf0a0092f67da28d6d66776680d5484"
                )))),
                address,
            ),
        )
        .await
        .unwrap_err();
    assert_matches!(err, Error::Call(CallError::Custom(err)) if err == ErrorObject::owned(
        JsonRpcError::InvalidBlockId as i32,
        JsonRpcError::InvalidBlockId.to_string(),
        None::<()>,
    ));

    // Ask for an invalid block number.
    let err = module
        .call::<_, ClassHash>(
            "starknet_getClassHashAt",
            (BlockId::HashOrNumber(BlockHashOrNumber::Number(BlockNumber(1))), address),
        )
        .await
        .unwrap_err();
    assert_matches!(err, Error::Call(CallError::Custom(err)) if err == ErrorObject::owned(
        JsonRpcError::InvalidBlockId as i32,
        JsonRpcError::InvalidBlockId.to_string(),
        None::<()>,
    ));
    Ok(())
}

#[tokio::test]
async fn test_get_transaction_by_hash() -> Result<(), anyhow::Error> {
    let (storage_reader, mut storage_writer) = test_utils::get_test_storage();
    let module = JsonRpcServerImpl { storage_reader }.into_rpc();

    let (_, body) = get_test_block(1);
    storage_writer.begin_rw_txn()?.append_body(BlockNumber(0), &body)?.commit()?;

    let expected_transaction = body.transactions.get(0).unwrap();
    let res = module
        .call::<_, TransactionWithType>(
            "starknet_getTransactionByHash",
            [expected_transaction.transaction_hash()],
        )
        .await
        .unwrap();
    assert_eq!(res, TransactionWithType::from(expected_transaction.clone()));

    // Ask for an invalid transaction.
    let err = module
        .call::<_, TransactionWithType>(
            "starknet_getTransactionByHash",
            [TransactionHash(StarkHash::from_u64(1))],
        )
        .await
        .unwrap_err();
    assert_matches!(err, Error::Call(CallError::Custom(err)) if err == ErrorObject::owned(
        JsonRpcError::InvalidTransactionHash as i32,
        JsonRpcError::InvalidTransactionHash.to_string(),
        None::<()>,
    ));
    Ok(())
}

#[tokio::test]
async fn test_get_transaction_by_block_id_and_index() -> Result<(), anyhow::Error> {
    let (storage_reader, mut storage_writer) = test_utils::get_test_storage();
    let module = JsonRpcServerImpl { storage_reader }.into_rpc();

    let (header, body) = get_test_block(1);
    storage_writer
        .begin_rw_txn()?
        .append_header(header.block_number, &header)?
        .append_body(header.block_number, &body)?
        .commit()?;

    let expected_transaction = body.transactions.get(0).unwrap();

    // Get transaction by block hash.
    let res = module
        .call::<_, TransactionWithType>(
            "starknet_getTransactionByBlockIdAndIndex",
            (BlockId::HashOrNumber(BlockHashOrNumber::Hash(header.block_hash)), 0),
        )
        .await
        .unwrap();
    assert_eq!(res, TransactionWithType::from(expected_transaction.clone()));

    // Get transaction by block number.
    let res = module
        .call::<_, TransactionWithType>(
            "starknet_getTransactionByBlockIdAndIndex",
            (BlockId::HashOrNumber(BlockHashOrNumber::Number(header.block_number)), 0),
        )
        .await
        .unwrap();
    assert_eq!(res, TransactionWithType::from(expected_transaction.clone()));

    // Ask for an invalid block hash.
    let err = module
        .call::<_, TransactionWithType>(
            "starknet_getTransactionByBlockIdAndIndex",
            (
                BlockId::HashOrNumber(BlockHashOrNumber::Hash(BlockHash(shash!(
                    "0x642b629ad8ce233b55798c83bb629a59bf0a0092f67da28d6d66776680d5484"
                )))),
                0,
            ),
        )
        .await
        .unwrap_err();
    assert_matches!(err, Error::Call(CallError::Custom(err)) if err == ErrorObject::owned(
        JsonRpcError::InvalidBlockId as i32,
        JsonRpcError::InvalidBlockId.to_string(),
        None::<()>,
    ));

    // Ask for an invalid block number.
    let err = module
        .call::<_, TransactionWithType>(
            "starknet_getTransactionByBlockIdAndIndex",
            (BlockId::HashOrNumber(BlockHashOrNumber::Number(BlockNumber(1))), 0),
        )
        .await
        .unwrap_err();
    assert_matches!(err, Error::Call(CallError::Custom(err)) if err == ErrorObject::owned(
        JsonRpcError::InvalidBlockId as i32,
        JsonRpcError::InvalidBlockId.to_string(),
        None::<()>,
    ));

    // Ask for an invalid transaction index.
    let err = module
        .call::<_, TransactionWithType>(
            "starknet_getTransactionByBlockIdAndIndex",
            (BlockId::HashOrNumber(BlockHashOrNumber::Hash(header.block_hash)), 1),
        )
        .await
        .unwrap_err();
    assert_matches!(err, Error::Call(CallError::Custom(err)) if err == ErrorObject::owned(
        JsonRpcError::InvalidTransactionIndex as i32,
        JsonRpcError::InvalidTransactionIndex.to_string(),
        None::<()>,
    ));
    Ok(())
}

#[tokio::test]
async fn test_get_block_transaction_count() -> Result<(), anyhow::Error> {
    let (storage_reader, mut storage_writer) = test_utils::get_test_storage();
    let module = JsonRpcServerImpl { storage_reader }.into_rpc();

    let transaction_count = 5;
    let (header, body) = get_test_block(transaction_count);
    storage_writer
        .begin_rw_txn()?
        .append_header(header.block_number, &header)?
        .append_body(header.block_number, &body)?
        .commit()?;

    // Get block by hash.
    let res = module
        .call::<_, usize>(
            "starknet_getBlockTransactionCount",
            [BlockId::HashOrNumber(BlockHashOrNumber::Hash(header.block_hash))],
        )
        .await?;
    assert_eq!(res, transaction_count);

    // Get block by number.
    let res = module
        .call::<_, usize>(
            "starknet_getBlockTransactionCount",
            [BlockId::HashOrNumber(BlockHashOrNumber::Number(header.block_number))],
        )
        .await?;
    assert_eq!(res, transaction_count);

    // Ask for the latest block.
    let res = module
        .call::<_, usize>("starknet_getBlockTransactionCount", [BlockId::Tag(Tag::Latest)])
        .await?;
    assert_eq!(res, transaction_count);

    // Ask for an invalid block hash.
    let err = module
        .call::<_, usize>(
            "starknet_getBlockTransactionCount",
            [BlockId::HashOrNumber(BlockHashOrNumber::Hash(BlockHash(shash!(
                "0x642b629ad8ce233b55798c83bb629a59bf0a0092f67da28d6d66776680d5484"
            ))))],
        )
        .await
        .unwrap_err();
    assert_matches!(err, Error::Call(CallError::Custom(err)) if err == ErrorObject::owned(
        JsonRpcError::InvalidBlockId as i32,
        JsonRpcError::InvalidBlockId.to_string(),
        None::<()>,
    ));

    // Ask for an invalid block number.
    let err = module
        .call::<_, usize>(
            "starknet_getBlockTransactionCount",
            [BlockId::HashOrNumber(BlockHashOrNumber::Number(BlockNumber(1)))],
        )
        .await
        .unwrap_err();
    assert_matches!(err, Error::Call(CallError::Custom(err)) if err == ErrorObject::owned(
        JsonRpcError::InvalidBlockId as i32,
        JsonRpcError::InvalidBlockId.to_string(),
        None::<()>,
    ));
    Ok(())
}

#[tokio::test]
async fn test_get_state_update() -> Result<(), anyhow::Error> {
    let (storage_reader, mut storage_writer) = test_utils::get_test_storage();
    let module = JsonRpcServerImpl { storage_reader }.into_rpc();

    let (parent_header, header, diff) = get_test_state_diff();
    storage_writer
        .begin_rw_txn()?
        .append_header(parent_header.block_number, &parent_header)?
        .append_state_diff(parent_header.block_number, &StateDiffForward::default())?
        .append_header(header.block_number, &header)?
        .append_state_diff(header.block_number, &diff)?
        .commit()?;

    let expected_update = StateUpdate {
        block_hash: header.block_hash,
        new_root: header.state_root,
        old_root: parent_header.state_root,
        state_diff: StateDiff {
            storage_diffs: from_starknet_storage_diffs(diff.storage_diffs),
            declared_contracts: vec![],
            deployed_contracts: diff.deployed_contracts,
            nonces: vec![],
        },
    };
    assert_eq!(expected_update.state_diff.storage_diffs.len(), 3);

    // Get state update by block hash.
    let res = module
        .call::<_, StateUpdate>(
            "starknet_getStateUpdate",
            [BlockId::HashOrNumber(BlockHashOrNumber::Hash(header.block_hash))],
        )
        .await?;
    assert_eq!(res, expected_update);

    // Get state update by block number.
    let res = module
        .call::<_, StateUpdate>(
            "starknet_getStateUpdate",
            [BlockId::HashOrNumber(BlockHashOrNumber::Number(header.block_number))],
        )
        .await?;
    assert_eq!(res, expected_update);

    // Ask for an invalid block hash.
    let err = module
        .call::<_, StateUpdate>(
            "starknet_getStateUpdate",
            [BlockId::HashOrNumber(BlockHashOrNumber::Hash(BlockHash(shash!(
                "0x642b629ad8ce233b55798c83bb629a59bf0a0092f67da28d6d66776680d5484"
            ))))],
        )
        .await
        .unwrap_err();
    assert_matches!(err, Error::Call(CallError::Custom(err)) if err == ErrorObject::owned(
        JsonRpcError::InvalidBlockId as i32,
        JsonRpcError::InvalidBlockId.to_string(),
        None::<()>,
    ));

    // Ask for an invalid block number.
    let err = module
        .call::<_, StateUpdate>(
            "starknet_getStateUpdate",
            [BlockId::HashOrNumber(BlockHashOrNumber::Number(BlockNumber(2)))],
        )
        .await
        .unwrap_err();
    assert_matches!(err, Error::Call(CallError::Custom(err)) if err == ErrorObject::owned(
        JsonRpcError::InvalidBlockId as i32,
        JsonRpcError::InvalidBlockId.to_string(),
        None::<()>,
    ));

    Ok(())
}

#[tokio::test]
async fn test_get_transaction_receipt() -> Result<(), anyhow::Error> {
    let (storage_reader, mut storage_writer) = test_utils::get_test_storage();
    let module = JsonRpcServerImpl { storage_reader }.into_rpc();

    let (_, body) = get_test_block(1);
    storage_writer.begin_rw_txn()?.append_body(BlockNumber(0), &body)?.commit()?;
    // TODO(anatg): Write a transaction receipt to the storage.

    let transaction_hash = body.transactions.get(0).unwrap().transaction_hash();
    let expected_receipt = TransactionReceipt::Declare(DeclareTransactionReceipt::default());
    let res = module
        .call::<_, TransactionReceipt>("starknet_getTransactionReceipt", [transaction_hash])
        .await
        .unwrap();
    assert_eq!(res, expected_receipt.clone());

    // Ask for an invalid transaction.
    let err = module
        .call::<_, TransactionReceipt>(
            "starknet_getTransactionReceipt",
            [TransactionHash(StarkHash::from_u64(1))],
        )
        .await
        .unwrap_err();
    assert_matches!(err, Error::Call(CallError::Custom(err)) if err == ErrorObject::owned(
        JsonRpcError::InvalidTransactionHash as i32,
        JsonRpcError::InvalidTransactionHash.to_string(),
        None::<()>,
    ));

    Ok(())
}

#[tokio::test]
async fn test_get_class() -> Result<(), anyhow::Error> {
    let (storage_reader, _) = test_utils::get_test_storage();
    let module = JsonRpcServerImpl { storage_reader }.into_rpc();

    // TODO(anatg): Write a contract class to the storage.

    let expected_contract_class = ContractClass::default();
    let res =
        module.call::<_, ContractClass>("starknet_getClass", [ClassHash::default()]).await.unwrap();
    assert_eq!(res, expected_contract_class.clone());

    // TODO(anatg): Ask for an invalid contract class.

    Ok(())
}

#[tokio::test]
async fn test_get_class_at() -> Result<(), anyhow::Error> {
    let (storage_reader, mut storage_writer) = test_utils::get_test_storage();
    let module = JsonRpcServerImpl { storage_reader }.into_rpc();

    // TODO(anatg): Write a contract class to the storage.
    let (header, _, diff) = get_test_state_diff();
    storage_writer
        .begin_rw_txn()?
        .append_header(header.block_number, &header)?
        .append_state_diff(header.block_number, &diff)?
        .commit()?;

    let address = diff.deployed_contracts.get(0).unwrap().address;
    let expected_contract_class = ContractClass::default();

    // Get class hash by block hash.
    let res = module
        .call::<_, ContractClass>(
            "starknet_getClassAt",
            (BlockId::HashOrNumber(BlockHashOrNumber::Hash(header.block_hash)), address),
        )
        .await?;
    assert_eq!(res, expected_contract_class);

    // Get class hash by block number.
    let res = module
        .call::<_, ContractClass>(
            "starknet_getClassAt",
            (BlockId::HashOrNumber(BlockHashOrNumber::Number(header.block_number)), address),
        )
        .await?;
    assert_eq!(res, expected_contract_class);

    // Ask for an invalid contract.
    let err = module
        .call::<_, ContractClass>(
            "starknet_getClassAt",
            (
                BlockId::HashOrNumber(BlockHashOrNumber::Number(header.block_number)),
                ContractAddress(shash!("0x12")),
            ),
        )
        .await
        .unwrap_err();
    assert_matches!(err, Error::Call(CallError::Custom(err)) if err == ErrorObject::owned(
        JsonRpcError::ContractNotFound as i32,
        JsonRpcError::ContractNotFound.to_string(),
        None::<()>,
    ));

    // Ask for an invalid block hash.
    let err = module
        .call::<_, ContractClass>(
            "starknet_getClassAt",
            (
                BlockId::HashOrNumber(BlockHashOrNumber::Hash(BlockHash(shash!(
                    "0x642b629ad8ce233b55798c83bb629a59bf0a0092f67da28d6d66776680d5484"
                )))),
                address,
            ),
        )
        .await
        .unwrap_err();
    assert_matches!(err, Error::Call(CallError::Custom(err)) if err == ErrorObject::owned(
        JsonRpcError::InvalidBlockId as i32,
        JsonRpcError::InvalidBlockId.to_string(),
        None::<()>,
    ));

    // Ask for an invalid block number.
    let err = module
        .call::<_, ContractClass>(
            "starknet_getClassAt",
            (BlockId::HashOrNumber(BlockHashOrNumber::Number(BlockNumber(1))), address),
        )
        .await
        .unwrap_err();
    assert_matches!(err, Error::Call(CallError::Custom(err)) if err == ErrorObject::owned(
        JsonRpcError::InvalidBlockId as i32,
        JsonRpcError::InvalidBlockId.to_string(),
        None::<()>,
    ));

    Ok(())
}

#[tokio::test]
async fn test_run_server() -> Result<(), anyhow::Error> {
    let (storage_reader, _) = test_utils::get_test_storage();
    let (addr, _handle) =
        run_server(GatewayConfig { server_ip: String::from("127.0.0.1:0") }, storage_reader)
            .await?;
    let client = HttpClientBuilder::default().build(format!("http://{:?}", addr))?;
    let err = client.block_number().await.unwrap_err();
    assert_matches!(err, Error::Call(CallError::Custom(err)) if err == ErrorObject::owned(
        JsonRpcError::NoBlocks as i32,
        JsonRpcError::NoBlocks.to_string(),
        None::<()>,
    ));
    Ok(())
}
