use cirrus_primitives::{BlockNumber, Hash, SecondaryApi};
use cirrus_test_service::run_primary_chain_validator_node;
use cirrus_test_service::runtime::{Header, UncheckedExtrinsic};
use cirrus_test_service::Keyring::{Alice, Bob, Ferdie};
use codec::{Decode, Encode};
use sc_client_api::{Backend, BlockBackend, HeaderBackend, StateBackend};
use sc_service::Role;
use sc_transaction_pool_api::TransactionSource;
use sp_api::ProvideRuntimeApi;
use sp_core::traits::FetchRuntimeCode;
use sp_core::Pair;
use sp_executor::{ExecutionPhase, ExecutorPair, FraudProof, SignedExecutionReceipt};
use sp_runtime::generic::{BlockId, DigestItem};
use sp_runtime::traits::{BlakeTwo256, Hash as HashT, Header as HeaderT};
use std::collections::HashSet;

#[substrate_test_utils::test(flavor = "multi_thread")]
async fn test_executor_full_node_catching_up() {
    let mut builder = sc_cli::LoggerBuilder::new("");
    builder.with_colors(false);
    let _ = builder.init();

    let tokio_handle = tokio::runtime::Handle::current();

    // Start Ferdie
    let (ferdie, ferdie_network_starter) =
        run_primary_chain_validator_node(tokio_handle.clone(), Ferdie, vec![]).await;
    ferdie_network_starter.start_network();

    // Run Alice (a secondary chain authority node)
    let alice = cirrus_test_service::TestNodeBuilder::new(tokio_handle.clone(), Alice)
        .connect_to_primary_chain_node(&ferdie)
        .build(Role::Authority, false, false)
        .await;

    // Run Bob (a secondary chain full node)
    let bob = cirrus_test_service::TestNodeBuilder::new(tokio_handle, Bob)
        .connect_to_primary_chain_node(&ferdie)
        .build(Role::Full, false, false)
        .await;

    // Bob is able to sync blocks.
    futures::future::join(alice.wait_for_blocks(3), bob.wait_for_blocks(3)).await;

    assert_eq!(
        ferdie.client.info().best_number,
        alice.client.info().best_number,
        "Primary chain and secondary chain must be on the same best height"
    );

    let alice_block_hash = alice
        .client
        .expect_block_hash_from_id(&BlockId::Number(2))
        .unwrap();
    let bob_block_hash = bob
        .client
        .expect_block_hash_from_id(&BlockId::Number(2))
        .unwrap();
    assert_eq!(
        alice_block_hash, bob_block_hash,
        "Executor authority node and full node must have the same state"
    );
}

// TODO: This test fails in CI, un-ignore once fixed
#[substrate_test_utils::test(flavor = "multi_thread")]
#[ignore]
async fn fraud_proof_verification_in_tx_pool_should_work() {
    let mut builder = sc_cli::LoggerBuilder::new("");
    builder.with_colors(false);
    let _ = builder.init();

    let tokio_handle = tokio::runtime::Handle::current();

    // Start Ferdie
    let (ferdie, ferdie_network_starter) =
        run_primary_chain_validator_node(tokio_handle.clone(), Ferdie, vec![]).await;
    ferdie_network_starter.start_network();

    // Run Alice (a secondary chain authority node)
    let alice = cirrus_test_service::TestNodeBuilder::new(tokio_handle.clone(), Alice)
        .connect_to_primary_chain_node(&ferdie)
        .build(Role::Authority, false, false)
        .await;

    alice.wait_for_blocks(3).await;

    let header = alice.client.header(&BlockId::Number(1)).unwrap().unwrap();
    let parent_header = alice
        .client
        .header(&BlockId::Hash(*header.parent_hash()))
        .unwrap()
        .unwrap();

    let intermediate_roots = alice
        .client
        .runtime_api()
        .intermediate_roots(&BlockId::Hash(header.hash()))
        .expect("Get intermediate roots");

    let prover = subspace_fraud_proof::ExecutionProver::new(
        alice.backend.clone(),
        alice.code_executor.clone(),
        Box::new(alice.task_manager.spawn_handle()),
    );

    let new_header = Header::new(
        *header.number(),
        Default::default(),
        Default::default(),
        parent_header.hash(),
        Default::default(),
    );
    let execution_phase = ExecutionPhase::InitializeBlock {
        call_data: new_header.encode(),
    };

    let storage_proof = prover
        .prove_execution::<sp_trie::PrefixedMemoryDB<BlakeTwo256>>(
            BlockId::Hash(parent_header.hash()),
            &execution_phase,
            None,
        )
        .expect("Create `initialize_block` proof");

    let header_ferdie = ferdie.client.header(&BlockId::Number(1)).unwrap().unwrap();
    let parent_hash_ferdie = header_ferdie.hash();
    let parent_number_ferdie = *header_ferdie.number();

    let valid_fraud_proof = FraudProof {
        bad_signed_receipt_hash: Hash::random(),
        parent_number: parent_number_ferdie,
        parent_hash: parent_hash_ferdie,
        pre_state_root: *parent_header.state_root(),
        post_state_root: intermediate_roots[0].into(),
        proof: storage_proof,
        execution_phase: execution_phase.clone(),
    };

    let tx = subspace_test_runtime::UncheckedExtrinsic::new_unsigned(
        pallet_executor::Call::submit_fraud_proof {
            fraud_proof: valid_fraud_proof.clone(),
        }
        .into(),
    );

    let expected_tx_hash = tx.using_encoded(BlakeTwo256::hash);
    let tx_hash = ferdie
        .transaction_pool
        .pool()
        .submit_one(
            &BlockId::Hash(ferdie.client.info().best_hash),
            TransactionSource::External,
            tx.into(),
        )
        .await
        .expect("Error at submitting a valid fraud proof");
    assert_eq!(tx_hash, expected_tx_hash);

    let invalid_fraud_proof = FraudProof {
        post_state_root: Hash::random(),
        ..valid_fraud_proof
    };

    let tx = subspace_test_runtime::UncheckedExtrinsic::new_unsigned(
        pallet_executor::Call::submit_fraud_proof {
            fraud_proof: invalid_fraud_proof,
        }
        .into(),
    );

    let submit_invalid_fraud_proof_result = ferdie
        .transaction_pool
        .pool()
        .submit_one(
            &BlockId::Hash(ferdie.client.info().best_hash),
            TransactionSource::External,
            tx.into(),
        )
        .await;

    match submit_invalid_fraud_proof_result.unwrap_err() {
        sc_transaction_pool::error::Error::Pool(
            sc_transaction_pool_api::error::Error::InvalidTransaction(invalid_tx),
        ) => assert_eq!(
            invalid_tx,
            sp_executor::InvalidTransactionCode::FraudProof.into()
        ),
        e => panic!("Unexpected error while submitting an invalid fraud proof: {e}"),
    }
}

// TODO: Add a new test which simulates a situation that an executor produces a fraud proof
// when an invalid receipt is received.

#[substrate_test_utils::test(flavor = "multi_thread")]
async fn set_new_code_should_work() {
    let mut builder = sc_cli::LoggerBuilder::new("");
    builder.with_colors(false);
    let _ = builder.init();

    let tokio_handle = tokio::runtime::Handle::current();

    // Start Ferdie
    let (ferdie, ferdie_network_starter) =
        run_primary_chain_validator_node(tokio_handle.clone(), Ferdie, vec![]).await;
    ferdie_network_starter.start_network();

    // Run Alice (a secondary chain authority node)
    let alice = cirrus_test_service::TestNodeBuilder::new(tokio_handle.clone(), Alice)
        .connect_to_primary_chain_node(&ferdie)
        .build(Role::Authority, false, false)
        .await;

    ferdie.wait_for_blocks(1).await;

    let new_runtime_wasm_blob = b"new_runtime_wasm_blob".to_vec();

    let best_number = alice.client.info().best_number;
    let primary_number = best_number + 1;
    // Although we're processing the bundle manually, the original bundle processor still works in
    // the meanwhile, it's possible the executor alice already processed this primary block, expecting next
    // primary block, in which case we use a dummy primary hash instead.
    //
    // Nice to disable the built-in bundle processor and have a full control of the executor block
    // production manually.
    let primary_hash = ferdie
        .client
        .hash(primary_number)
        .unwrap()
        .unwrap_or_else(Hash::random);
    alice
        .executor
        .clone()
        .process_bundles(
            (primary_hash, primary_number),
            Default::default(),
            BlakeTwo256::hash_of(&[1u8; 64]).into(),
            Some(new_runtime_wasm_blob.clone().into()),
        )
        .await;

    let best_hash = alice.client.info().best_hash;
    let state = alice
        .backend
        .state_at(BlockId::Hash(best_hash))
        .expect("Get state");
    let trie_backend = state.as_trie_backend().unwrap();
    let state_runtime_code = sp_state_machine::backend::BackendRuntimeCode::new(trie_backend);
    let runtime_code = state_runtime_code.fetch_runtime_code().unwrap();
    let logs = alice
        .client
        .header(&BlockId::Hash(best_hash))
        .unwrap()
        .unwrap()
        .digest
        .logs;
    if logs != vec![DigestItem::RuntimeEnvironmentUpdated] {
        let extrinsics = alice
            .client
            .block_body(&BlockId::Hash(best_hash))
            .unwrap()
            .unwrap()
            .into_iter()
            .map(|encoded_extrinsic| {
                UncheckedExtrinsic::decode(&mut encoded_extrinsic.encode().as_slice()).unwrap()
            })
            .collect::<Vec<_>>();
        panic!("`set_code` not executed, extrinsics in the block: {extrinsics:?}")
    }
    assert_eq!(runtime_code, new_runtime_wasm_blob);
}

#[substrate_test_utils::test(flavor = "multi_thread")]
async fn pallet_executor_unsigned_extrinsics_should_work() {
    let mut builder = sc_cli::LoggerBuilder::new("");
    builder.with_colors(false);
    let _ = builder.init();

    let tokio_handle = tokio::runtime::Handle::current();

    // Start Ferdie
    let (ferdie, ferdie_network_starter) =
        run_primary_chain_validator_node(tokio_handle.clone(), Ferdie, vec![]).await;
    ferdie_network_starter.start_network();

    // Run Alice (a secondary chain full node)
    // Run a full node deliberately in order to control the execution chain by
    // submitting the receipts manually later.
    let alice = cirrus_test_service::TestNodeBuilder::new(tokio_handle.clone(), Alice)
        .connect_to_primary_chain_node(&ferdie)
        .build(Role::Full, false, false)
        .await;

    alice.wait_for_blocks(2).await;

    // Wait for one more block to make sure the execution receipts of block 1,2 are
    // able to be written to the database.
    alice.wait_for_blocks(1).await;

    let create_and_send_submit_execution_receipt = |primary_number: BlockNumber| {
        let execution_receipt = crate::aux_schema::load_execution_receipt(
            &*alice.backend,
            alice.client.hash(primary_number).unwrap().unwrap(),
        )
        .expect("Failed to load execution receipt from the local aux_db")
        .unwrap_or_else(|| {
            panic!("The requested execution receipt for block {primary_number} does not exist")
        });

        let pair = ExecutorPair::from_string("//Alice", None).unwrap();
        let signature = pair.sign(execution_receipt.hash().as_ref());
        let signer = pair.public();

        let signed_execution_receipt = SignedExecutionReceipt {
            execution_receipt,
            signature,
            signer,
        };

        let tx = subspace_test_runtime::UncheckedExtrinsic::new_unsigned(
            pallet_executor::Call::submit_execution_receipt {
                signed_execution_receipt,
            }
            .into(),
        );

        let pool = ferdie.transaction_pool.pool();
        let ferdie_best_hash = ferdie.client.info().best_hash;

        async move {
            pool.submit_one(
                &BlockId::Hash(ferdie_best_hash),
                TransactionSource::External,
                tx.into(),
            )
            .await
        }
    };

    let ready_txs = || {
        ferdie
            .transaction_pool
            .pool()
            .validated_pool()
            .ready()
            .map(|tx| tx.hash)
            .collect::<Vec<_>>()
    };

    let (tx1, tx2) = futures::join!(
        create_and_send_submit_execution_receipt(1),
        create_and_send_submit_execution_receipt(2),
    );
    assert_eq!(vec![tx1.unwrap(), tx2.unwrap()], ready_txs());

    // Wait for up to 5 blocks to ensure the ready txs can be consumed.
    for _ in 0..5 {
        alice.wait_for_blocks(1).await;
        if ready_txs().is_empty() {
            break;
        }
    }
    assert!(ready_txs().is_empty());

    alice.wait_for_blocks(2).await;

    let future_txs = || {
        ferdie
            .transaction_pool
            .pool()
            .validated_pool()
            .futures()
            .into_iter()
            .map(|(tx_hash, _)| tx_hash)
            .collect::<HashSet<_>>()
    };
    // best execution chain number is 2, receipt for #4 will be put into the futures queue.
    let tx4 = create_and_send_submit_execution_receipt(4)
        .await
        .expect("Submit a future receipt successfully");
    assert_eq!(HashSet::from([tx4]), future_txs());

    // max drift is 2, hence the max allowed receipt number is 2 + 2, 5 will be rejected as being
    // too far.
    match create_and_send_submit_execution_receipt(5)
        .await
        .unwrap_err()
    {
        sc_transaction_pool::error::Error::Pool(
            sc_transaction_pool_api::error::Error::InvalidTransaction(invalid_tx),
        ) => assert_eq!(
            invalid_tx,
            sp_executor::InvalidTransactionCode::ExecutionReceipt.into()
        ),
        e => panic!("Unexpected error while submitting execution receipt: {e}"),
    }

    let tx3 = create_and_send_submit_execution_receipt(3)
        .await
        .expect("Submit receipt 3 successfully");
    // All future txs become ready once the required tx is ready.
    assert_eq!(vec![tx3, tx4], ready_txs());
    assert!(future_txs().is_empty());

    // Wait for up to 5 blocks to ensure the ready txs can be consumed.
    for _ in 0..5 {
        alice.wait_for_blocks(1).await;
        if ready_txs().is_empty() {
            break;
        }
    }
    assert!(ready_txs().is_empty());
}
