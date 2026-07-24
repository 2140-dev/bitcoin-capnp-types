use bitcoin_capnp_types::{
    init_capnp::init,
    mining_capnp,
    proxy_capnp::{thread, thread_map},
};
use capnp_rpc::{RpcSystem, rpc_twoparty_capnp::Side};
use encoding::encode_to_vec;
use tokio::task::LocalSet;

mod util;

use serde_json::{Value, json};
use util::bitcoin_core::{
    connect_unix_stream, destroy_template, make_block_template, mempool_tx_count, unix_socket_path,
    with_init_client, with_mining_client, with_rpc_client,
};
use util::bitcoin_core_wallet::{
    bitcoin_test_wallet, create_mempool_self_transfer, ensure_wallet_loaded_and_funded,
};
use util::block::{block_solution, block_with_pow};

struct SubmitOutcome {
    accepted: bool,
    reason: String,
    debug: String,
}

async fn submit_block(mining: &mining_capnp::mining::Client, block: &[u8]) -> SubmitOutcome {
    let mut req = mining.submit_block_request();
    req.get().set_block(block);
    let resp = req.send().promise.await.unwrap();
    let results = resp.get().unwrap();
    SubmitOutcome {
        accepted: results.get_result(),
        reason: results.get_reason().unwrap().to_string().unwrap(),
        debug: results.get_debug().unwrap().to_string().unwrap(),
    }
}

async fn submit_solution(
    template: &mining_capnp::block_template::Client,
    solution: &util::block::BlockSolution,
) -> SubmitOutcome {
    let mut req = template.submit_solution_request();
    {
        let mut params = req.get();
        params.set_version(solution.version);
        params.set_timestamp(solution.timestamp);
        params.set_nonce(solution.nonce);
        params.set_coinbase(&solution.coinbase);
    }
    let resp = req.send().promise.await.unwrap();
    let results = resp.get().unwrap();
    SubmitOutcome {
        accepted: results.get_result(),
        reason: results.get_reason().unwrap().to_string().unwrap(),
        debug: results.get_debug().unwrap().to_string().unwrap(),
    }
}

async fn get_template_block(template: &mining_capnp::block_template::Client) -> Vec<u8> {
    let resp = template.get_block_request().send().promise.await.unwrap();
    resp.get().unwrap().get_result().unwrap().to_vec()
}

#[tokio::test]
#[serial_test::parallel]
async fn integration() {
    with_init_client(|client| async move {
        let echo_client_request = client.make_echo_request().send().promise.await.unwrap();
        let echo_client = echo_client_request.get().unwrap().get_result().unwrap();
        let mut echo_conf = echo_client.echo_request();
        echo_conf.get().set_echo("Hello world");
        let echo_response = echo_conf.send().promise.await.unwrap();
        let text = echo_response
            .get()
            .unwrap()
            .get_result()
            .unwrap()
            .to_string()
            .unwrap();
        assert_eq!("Hello world", text);
    })
    .await;
}

/// Test the RPC interface by calling `uptime`
#[tokio::test]
#[serial_test::parallel]
async fn rpc_query_uptime() {
    with_rpc_client(|_client, rpc| async move {
        let mut execute_rpc_request = rpc.execute_rpc_request();
        let j: Value = json!({
            "jsonrpc": "2.0",
            "id": "test",
            "method": "uptime",
            "params": [],
        });
        execute_rpc_request.get().set_request(j.to_string());
        let exec_rpc_response = execute_rpc_request.send().promise.await.unwrap();
        let result = exec_rpc_response
            .get()
            .unwrap()
            .get_result()
            .unwrap()
            .to_string()
            .unwrap();
        let v: Value = serde_json::from_str(&result)
            .map_err(|e| format!("failed to parse rpc response as JSON: {e}"))
            .unwrap();
        let uptime = v["result"].as_i64().unwrap();
        assert!(uptime > 0, "Uptime must be greater than zero");
    })
    .await;
}

/// Calling the deprecated makeMiningOld2 (@2) should return an error from the
/// server. Cap'n Proto requires sequential ordinals so this placeholder cannot
/// be removed, but the server intentionally rejects it.
#[tokio::test]
#[serial_test::parallel]
async fn make_mining_old2_rejected() {
    with_init_client(|client| async move {
        let result = client.make_mining_old2_request().send().promise.await;
        assert!(
            result.is_err(),
            "makeMiningOld2 should be rejected by the server"
        );
    })
    .await;
}

/// Check the four mining constants from the capnp schema.
#[test]
#[serial_test::parallel]
fn mining_constants() {
    assert_eq!(mining_capnp::MAX_MONEY, 2_100_000_000_000_000i64);
    const { assert!(mining_capnp::MAX_DOUBLE > 1e300) };
    assert_eq!(mining_capnp::DEFAULT_BLOCK_RESERVED_WEIGHT, 8_000u32);
    assert_eq!(
        mining_capnp::DEFAULT_COINBASE_OUTPUT_MAX_ADDITIONAL_SIGOPS,
        400u32
    );
}

/// isTestChain, isInitialBlockDownload, getTip, getMemoryLoad.
#[tokio::test]
#[serial_test::parallel]
async fn mining_basic_queries() {
    with_mining_client(|_client, mining| async move {
        // isTestChain
        let resp = mining.is_test_chain_request().send().promise.await.unwrap();
        assert!(resp.get().unwrap().get_result(), "regtest is a test chain");

        // isInitialBlockDownload
        let resp = mining
            .is_initial_block_download_request()
            .send()
            .promise
            .await
            .unwrap();
        let _ibd = resp
            .get()
            .expect("isInitialBlockDownload response should decode")
            .get_result();

        // getTip
        let resp = mining.get_tip_request().send().promise.await.unwrap();
        let results = resp.get().unwrap();
        assert!(results.get_has_result(), "node should have a tip");
        let tip = results.get_result().unwrap();
        let tip_hash = tip.get_hash().unwrap();
        assert_eq!(tip_hash.len(), 32, "block hash must be 32 bytes");
        assert!(tip.get_height() >= 0, "height must be non-negative");

        // getMemoryLoad
        let resp = mining
            .get_memory_load_request()
            .send()
            .promise
            .await
            .unwrap();
        let memory_load = resp.get().unwrap().get_result().unwrap();
        let _usage: u64 = memory_load.get_usage();
    })
    .await;
}

/// waitTipChanged with a short timeout.
#[tokio::test]
// Serialized because this assertion is sensitive to concurrent tip changes.
#[serial_test::serial]
async fn mining_wait_tip_changed() {
    with_mining_client(|_client, mining| async move {
        // Get the current tip first.
        let resp = mining.get_tip_request().send().promise.await.unwrap();
        let results = resp.get().unwrap();
        let tip = results.get_result().unwrap();
        let tip_hash: Vec<u8> = tip.get_hash().unwrap().to_vec();
        let tip_height: i32 = tip.get_height();

        // Wait with a short timeout; no new block should arrive.
        let mut req = mining.wait_tip_changed_request();
        req.get().set_current_tip(&tip_hash);
        req.get().set_timeout(500.0); // 500 ms
        let resp = req.send().promise.await.unwrap();
        let wait_result = resp.get().unwrap().get_result().unwrap();
        assert_eq!(wait_result.get_hash().unwrap().len(), 32);
        assert_eq!(wait_result.get_height(), tip_height);
    })
    .await;
}

/// createNewBlock + all BlockTemplate read methods: getBlockHeader, getBlock,
/// getTxFees, getTxSigops, getCoinbaseTx, getCoinbaseMerklePath.
#[tokio::test]
#[serial_test::parallel]
async fn mining_block_template_inspection() {
    with_mining_client(|_client, mining| async move {
        let template = make_block_template(&mining).await;

        // getBlockHeader
        let resp = template
            .get_block_header_request()
            .send()
            .promise
            .await
            .unwrap();
        let header = resp.get().unwrap().get_result().unwrap();
        assert_eq!(header.len(), 80, "block header must be 80 bytes");

        // getBlock
        let resp = template.get_block_request().send().promise.await.unwrap();
        let block = resp.get().unwrap().get_result().unwrap();
        assert!(block.len() > 80, "serialized block must be > 80 bytes");

        // getTxFees
        let resp = template.get_tx_fees_request().send().promise.await.unwrap();
        let _fees = resp
            .get()
            .expect("getTxFees response should decode")
            .get_result()
            .expect("getTxFees response should contain fees");

        // getTxSigops
        let resp = template
            .get_tx_sigops_request()
            .send()
            .promise
            .await
            .unwrap();
        let _sigops = resp
            .get()
            .expect("getTxSigops response should decode")
            .get_result()
            .expect("getTxSigops response should contain sigops");

        // getCoinbaseTx — inspect every CoinbaseTx field
        let resp = template
            .get_coinbase_tx_request()
            .send()
            .promise
            .await
            .unwrap();
        let coinbase = resp.get().unwrap().get_result().unwrap();
        let _version: u32 = coinbase.get_version();
        let _sequence: u32 = coinbase.get_sequence();
        let script_sig_prefix = coinbase.get_script_sig_prefix().unwrap();
        assert!(
            !script_sig_prefix.is_empty(),
            "scriptSigPrefix must contain at least the block height"
        );
        let _witness = coinbase
            .get_witness()
            .expect("coinbase witness should decode");
        let reward: i64 = coinbase.get_block_reward_remaining();
        assert!(reward > 0 && reward <= mining_capnp::MAX_MONEY);
        let _required_outputs = coinbase
            .get_required_outputs()
            .expect("coinbase required outputs should decode");
        let _lock_time: u32 = coinbase.get_lock_time();

        // getCoinbaseMerklePath
        let resp = template
            .get_coinbase_merkle_path_request()
            .send()
            .promise
            .await
            .unwrap();
        let _merkle_path = resp
            .get()
            .expect("getCoinbaseMerklePath response should decode")
            .get_result()
            .expect("getCoinbaseMerklePath response should contain a merkle path");

        destroy_template(&template).await;
    })
    .await;
}

/// waitNext (short timeout), interruptWait, submitSolution (garbage), destroy.
#[tokio::test]
// Serialized because submitSolution behavior depends on current chain tip.
#[serial_test::serial]
async fn mining_block_template_lifecycle() {
    with_mining_client(|_client, mining| async move {
        let template = make_block_template(&mining).await;

        // waitNext — short timeout, no new transactions expected.
        let mut req = template.wait_next_request();
        {
            let mut opts = req.get().init_options();
            opts.set_timeout(100.0); // 100 ms
            opts.set_fee_threshold(mining_capnp::MAX_MONEY);
        }
        let resp = req.send().promise.await.unwrap();
        let results = resp.get().expect("waitNext response should decode");
        assert!(
            !results.has_result(),
            "waitNext should time out without a new template"
        );

        // interruptWait — should not crash.
        template
            .interrupt_wait_request()
            .send()
            .promise
            .await
            .expect("interruptWait should not fail");

        // submitSolution — garbage coinbase should be rejected.
        // This mutates the template, so we do it right before destroy.
        let mut req = template.submit_solution_request();
        req.get().set_version(1);
        req.get().set_timestamp(0);
        req.get().set_nonce(0);
        req.get().set_coinbase(&[0u8; 64]);
        let resp = req.send().promise.await.unwrap();
        let submitted = resp.get().unwrap().get_result();
        assert!(!submitted, "garbage solution must not be accepted");

        destroy_template(&template).await;
    })
    .await;
}

/// submitSolution with insufficient PoW should return reason/debug details.
#[tokio::test]
#[serial_test::serial]
async fn mining_block_template_submit_solution_insufficient_pow() {
    with_mining_client(|_client, mining| async move {
        let template = make_block_template(&mining).await;

        let block = get_template_block(&template).await;
        let block = block_with_pow(&block, false);
        let solution = block_solution(&block);

        let outcome = submit_solution(&template, &solution).await;
        assert!(
            !outcome.accepted,
            "solution with insufficient PoW must not be accepted"
        );
        assert_eq!(outcome.reason, "high-hash");
        assert_eq!(outcome.debug, "proof of work failed");

        destroy_template(&template).await;
    })
    .await;
}

/// submitSolution with a solved template block should be accepted.
#[tokio::test]
#[serial_test::serial]
async fn mining_block_template_submit_solution_resolved_and_duplicate() {
    with_mining_client(|_client, mining| async move {
        let template = make_block_template(&mining).await;

        let block = get_template_block(&template).await;
        let block = block_with_pow(&block, true);
        let solution = block_solution(&block);

        let outcome = submit_solution(&template, &solution).await;
        assert!(
            outcome.accepted,
            "solved template solution must be accepted: reason={}, debug={}",
            outcome.reason, outcome.debug
        );
        assert_eq!(outcome.reason, "");
        assert_eq!(outcome.debug, "");

        let outcome = submit_solution(&template, &solution).await;
        assert!(!outcome.accepted, "duplicate solution must not be accepted");
        assert_eq!(outcome.reason, "duplicate");
        assert_eq!(outcome.debug, "");

        destroy_template(&template).await;
    })
    .await;
}

/// submitBlock with insufficient PoW should be rejected.
#[tokio::test]
#[serial_test::serial]
async fn mining_submit_block_insufficient_pow() {
    with_mining_client(|_client, mining| async move {
        let template = make_block_template(&mining).await;

        let block = get_template_block(&template).await;
        let block = block_with_pow(&block, false);

        let outcome = submit_block(&mining, &block).await;
        assert!(
            !outcome.accepted,
            "block with insufficient PoW must not be accepted"
        );
        assert_eq!(outcome.reason, "high-hash");
        assert_eq!(outcome.debug, "proof of work failed");

        destroy_template(&template).await;
    })
    .await;
}

/// submitBlock with invalid contents should be rejected even with sufficient PoW.
#[tokio::test]
#[serial_test::serial]
async fn mining_submit_block_invalid() {
    with_mining_client(|_client, mining| async move {
        let template = make_block_template(&mining).await;

        let block = get_template_block(&template).await;
        let mut block = block_with_pow(&block, true);
        // Corrupt the serialized block after solving its header. This keeps
        // the PoW valid while making the header's Merkle root stale.
        *block
            .last_mut()
            .expect("serialized block must not be empty") ^= 1;

        let outcome = submit_block(&mining, &block).await;
        assert!(
            !outcome.accepted,
            "invalid block with sufficient PoW must not be accepted"
        );
        assert_eq!(outcome.reason, "bad-txnmrklroot");
        assert_eq!(outcome.debug, "hashMerkleRoot mismatch");

        destroy_template(&template).await;
    })
    .await;
}

/// submitBlock with a solved template block should be accepted.
#[tokio::test]
#[serial_test::serial]
async fn mining_submit_block_resolved() {
    with_mining_client(|_client, mining| async move {
        let template = make_block_template(&mining).await;

        let block = get_template_block(&template).await;
        let block = block_with_pow(&block, true);

        let outcome = submit_block(&mining, &block).await;
        assert!(
            outcome.accepted,
            "solved template block must be accepted: reason={}, debug={}",
            outcome.reason, outcome.debug
        );
        assert_eq!(outcome.reason, "");
        assert_eq!(outcome.debug, "");

        destroy_template(&template).await;
    })
    .await;
}

/// submitBlock with a duplicate solved block should be rejected.
#[tokio::test]
#[serial_test::serial]
async fn mining_submit_block_duplicate() {
    with_mining_client(|_client, mining| async move {
        let template = make_block_template(&mining).await;

        let block = get_template_block(&template).await;
        let block = block_with_pow(&block, true);

        let outcome = submit_block(&mining, &block).await;
        assert!(
            outcome.accepted,
            "first solved block submission must be accepted: reason={}, debug={}",
            outcome.reason, outcome.debug
        );
        assert_eq!(outcome.reason, "");
        assert_eq!(outcome.debug, "");

        let outcome = submit_block(&mining, &block).await;
        assert!(!outcome.accepted, "duplicate block must not be accepted");
        assert_eq!(outcome.reason, "duplicate");
        assert_eq!(outcome.debug, "");

        destroy_template(&template).await;
    })
    .await;
}

/// getTransactionsByTxID and getTransactionsByWitnessID with empty lists and
/// with a non-existent txid/wtxid.
#[tokio::test]
// Serialized because this test may mine blocks to recover wallet funding.
#[serial_test::serial]
async fn mining_get_transactions() {
    with_mining_client(|_client, mining| async move {
        let wallet = bitcoin_test_wallet();
        ensure_wallet_loaded_and_funded(&wallet);

        let real_tx = create_mempool_self_transfer(&wallet);
        let real_txid = real_tx.compute_txid().to_byte_array();
        let real_wtxid = real_tx.compute_wtxid().to_byte_array();
        let real_raw_tx = encode_to_vec(&real_tx);

        // getTransactionsByTxID — empty list should return empty list.
        let mut req = mining.get_transactions_by_tx_i_d_request();
        req.get().init_txids(0);
        let resp = req.send().promise.await.unwrap();
        let results = resp.get().unwrap().get_result().unwrap();
        assert_eq!(
            results.len(),
            0,
            "empty txid list should return empty results"
        );

        // getTransactionsByTxID — return real mempool tx and empty for unknown id.
        let fake_txid = [0x42u8; 32];
        let mut req = mining.get_transactions_by_tx_i_d_request();
        {
            let mut txids = req.get().init_txids(2);
            txids.set(0, &real_txid);
            txids.set(1, &fake_txid);
        }
        let resp = req.send().promise.await.unwrap();
        let results = resp.get().unwrap().get_result().unwrap();
        assert_eq!(
            results.len(),
            2,
            "should return one entry per requested txid, including misses"
        );
        assert_eq!(
            results.get(0).unwrap(),
            real_raw_tx.as_slice(),
            "known txid should return serialized transaction"
        );
        assert!(
            results.get(1).unwrap().is_empty(),
            "non-existent txid should return empty data"
        );

        // getTransactionsByWitnessID — empty list should return empty list.
        let mut req = mining.get_transactions_by_witness_i_d_request();
        req.get().init_wtxids(0);
        let resp = req.send().promise.await.unwrap();
        let results = resp.get().unwrap().get_result().unwrap();
        assert_eq!(
            results.len(),
            0,
            "empty wtxid list should return empty results"
        );

        // getTransactionsByWitnessID — return real mempool tx and empty for unknown id.
        let fake_wtxid = [0x43u8; 32];
        let mut req = mining.get_transactions_by_witness_i_d_request();
        {
            let mut wtxids = req.get().init_wtxids(2);
            wtxids.set(0, &real_wtxid);
            wtxids.set(1, &fake_wtxid);
        }
        let resp = req.send().promise.await.unwrap();
        let results = resp.get().unwrap().get_result().unwrap();
        assert_eq!(
            results.len(),
            2,
            "should return one entry per requested wtxid, including misses"
        );
        assert_eq!(
            results.get(0).unwrap(),
            real_raw_tx.as_slice(),
            "known wtxid should return serialized transaction"
        );
        assert!(
            results.get(1).unwrap().is_empty(),
            "non-existent wtxid should return empty data"
        );
    })
    .await;
}

/// checkBlock with a template block payload, and interrupt.
#[tokio::test]
// Serialized because interrupt() can affect other in-flight mining waits.
#[serial_test::serial]
async fn mining_check_block_and_interrupt() {
    with_mining_client(|_client, mining| async move {
        let template = make_block_template(&mining).await;

        let block = get_template_block(&template).await;

        // checkBlock should either error or return a response.
        let mut req = mining.check_block_request();
        req.get().set_block(&block);
        {
            let mut opts = req.get().init_options();
            opts.set_check_merkle_root(true);
            opts.set_check_pow(false);
        }
        let result = req.send().promise.await;
        match result {
            Ok(resp) => {
                let results = resp.get().expect("checkBlock response should decode");
                let _valid = results.get_result();
                let _reason = results
                    .get_reason()
                    .expect("checkBlock response should contain reason");
                let _debug = results
                    .get_debug()
                    .expect("checkBlock response should contain debug");
            }
            Err(_) => {
                // Server may reject validation/deserialization.
            }
        }

        destroy_template(&template).await;

        // interrupt — should not crash.
        mining
            .interrupt_request()
            .send()
            .promise
            .await
            .expect("interrupt should not fail");
    })
    .await;
}

/// Exercise the `context.thread` dispatch path. Clients may want to use this if they know a call
/// will block for a long time, potentially indefinitely.
#[tokio::test]
#[serial_test::parallel]
async fn echo_with_explicit_thread() {
    let rpc_network = connect_unix_stream(unix_socket_path()).await;
    let mut rpc_system = RpcSystem::new(Box::new(rpc_network), None);
    LocalSet::new()
        .run_until(async move {
            let client: init::Client = rpc_system.bootstrap(Side::Server);
            tokio::task::spawn_local(rpc_system);

            let construct_resp = client
                .construct_request()
                .send()
                .promise
                .await
                .expect("could not create initial request");
            let thread_map: thread_map::Client =
                construct_resp.get().unwrap().get_thread_map().unwrap();
            let thread_resp = thread_map
                .make_thread_request()
                .send()
                .promise
                .await
                .unwrap();
            let thread: thread::Client = thread_resp.get().unwrap().get_result().unwrap();

            let mut make_echo = client.make_echo_request();
            make_echo
                .get()
                .get_context()
                .unwrap()
                .set_thread(thread.clone());
            let echo_resp = make_echo.send().promise.await.unwrap();
            let echo = echo_resp.get().unwrap().get_result().unwrap();

            let mut req = echo.echo_request();
            req.get().get_context().unwrap().set_thread(thread.clone());
            req.get().set_echo("thread-dispatched");
            let resp = req.send().promise.await.unwrap();
            let text = resp
                .get()
                .unwrap()
                .get_result()
                .unwrap()
                .to_string()
                .unwrap();
            assert_eq!("thread-dispatched", text);
        })
        .await;
}

/// Minimal coverage for wallet/mempool helpers added for future mempool tests.
#[tokio::test]
#[serial_test::serial]
async fn wallet_helpers_create_mempool_transaction() {
    let wallet = bitcoin_test_wallet();
    assert!(!wallet.is_empty(), "test wallet name must not be empty");

    ensure_wallet_loaded_and_funded(&wallet);
    let before = mempool_tx_count();
    let _tx = create_mempool_self_transfer(&wallet);
    let after = mempool_tx_count();
    assert_eq!(
        after,
        before + 1,
        "self-transfer should add one mempool transaction"
    );
}
