use bitcoin_capnp_types::mining_capnp::{self, mining};

#[path = "util/bitcoin_core.rs"]
mod bitcoin_core_util;
#[path = "util/bitcoin_core_wallet.rs"]
mod bitcoin_core_wallet_util;

use bitcoin_core_util::{
    destroy_template, make_block_template, mempool_tx_count, with_init_client,
    with_init_client_and_thread_map, with_mining_client,
};
use bitcoin_core_wallet_util::{
    bitcoin_test_wallet, create_mempool_self_transfer, ensure_wallet_loaded_and_funded,
};

#[tokio::test]
#[serial_test::parallel]
async fn integration() {
    with_init_client(|client, thread| async move {
        let mut echo = client.make_echo_request();
        echo.get().get_context().unwrap().set_thread(thread.clone());
        let echo_client_request = echo.send().promise.await.unwrap();
        let echo_client = echo_client_request.get().unwrap().get_result().unwrap();
        let mut echo_conf = echo_client.echo_request();
        echo_conf
            .get()
            .get_context()
            .unwrap()
            .set_thread(thread.clone());
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

/// makeEcho without setting context.thread.
///
/// On an upstream Bitcoin Core (libmultiprocess without `ThreadMap.makePool`)
/// the server-side dispatcher rejects requests whose context has no thread
/// handle — `m_threads.getLocalServer()` returns null and the handler throws
/// "invalid thread handle". This test pins that behavior so we notice if the
/// pooled-threads fork ever changes it (e.g., once `makePool` is wired up and
/// the server falls back to a pool instead of erroring).
#[tokio::test]
#[serial_test::parallel]
async fn make_echo_without_thread_errors() {
    with_init_client(|client, _thread| async move {
        let echo = client.make_echo_request();
        // Intentionally do NOT call set_thread on the context — leave the
        // thread capability null.
        let result = echo.send().promise.await;
        match &result {
            Ok(_) => {}
            Err(e) => eprintln!("makeEcho without thread errored as expected: {e}"),
        }
        assert!(
            result.is_err(),
            "makeEcho with no context.thread should be rejected by the server"
        );
    })
    .await;
}

/// Create a server-side thread pool via `ThreadMap.makePool`, then issue an
/// echo round-trip without setting `context.thread`. The server's dispatcher
/// should round-robin the call onto a pool thread and return the echoed text.
///
/// This requires a Bitcoin Core built against the pooled-threads libmultiprocess
/// fork — upstream rejects ordinal @1 on `ThreadMap` as "method not implemented".
#[tokio::test]
#[serial_test::parallel]
async fn make_echo_via_pool() {
    with_init_client_and_thread_map(|client, thread_map, _thread| async move {
        // Pre-allocate two pool threads on the server.
        let mut pool_req = thread_map.make_pool_request();
        pool_req.get().set_count(2);
        pool_req
            .send()
            .promise
            .await
            .expect("makePool should succeed on a pool-capable server");

        // Obtain an Echo capability. makeEcho itself dispatches via Context,
        // so leave thread unset here too — pool should service it.
        let make_echo = client.make_echo_request();
        let echo_response = make_echo
            .send()
            .promise
            .await
            .expect("makeEcho without thread should be dispatched via the pool");
        let echo_client = echo_response.get().unwrap().get_result().unwrap();

        // Now call echo() on the Echo capability — again without setting
        // context.thread — and assert the round-trip succeeds.
        let mut echo_call = echo_client.echo_request();
        echo_call.get().set_echo("Hello from pool");
        let echo_resp = echo_call
            .send()
            .promise
            .await
            .expect("echo without thread should be dispatched via the pool");
        let text = echo_resp
            .get()
            .unwrap()
            .get_result()
            .unwrap()
            .to_string()
            .unwrap();
        assert_eq!("Hello from pool", text);
    })
    .await;
}

/// Drive the Mining interface entirely through a server-side thread pool.
///
/// Creates a 4-thread pool, then obtains a Mining capability without setting
/// `context.thread`. Fires two batches of concurrent requests — 4 calls each,
/// none with a thread set — and awaits them with `try_join_all`. Every call
/// should be round-robin dispatched onto pool threads and succeed.
///
/// Requires a Bitcoin Core built against the pooled-threads libmultiprocess
/// fork (ordinal @1 on `ThreadMap`).
#[tokio::test]
#[serial_test::parallel]
async fn mining_via_pool_concurrent_queries() {
    with_init_client_and_thread_map(|client, thread_map, _thread| async move {
        // Pre-allocate 4 server threads. With 4 concurrent in-flight requests
        // per batch, each pool thread gets exactly one — exercising every
        // entry in the round-robin cursor.
        let mut pool_req = thread_map.make_pool_request();
        pool_req.get().set_count(4);
        pool_req
            .send()
            .promise
            .await
            .expect("makePool should succeed on a pool-capable server");

        // Obtain Mining via the pool (no thread set on makeMining's context).
        let make_mining = client.make_mining_request();
        let mining_resp = make_mining
            .send()
            .promise
            .await
            .expect("makeMining without thread should be dispatched via the pool");
        let mining: mining::Client = mining_resp.get().unwrap().get_result().unwrap();

        // Batch 1: four concurrent is_test_chain calls.
        let test_chain_futs: Vec<_> = (0..4)
            .map(|_| mining.is_test_chain_request().send().promise)
            .collect();
        let test_chain_results = futures::future::try_join_all(test_chain_futs)
            .await
            .expect("concurrent is_test_chain should all dispatch via pool");
        for resp in &test_chain_results {
            assert!(
                resp.get().unwrap().get_result(),
                "regtest is a test chain — all four pool threads should agree"
            );
        }

        // Batch 2: four concurrent get_tip calls. The node isn't mining during
        // tests, so each call should report a well-formed tip — but we don't
        // assert hash equality across calls in case a parallel test ever does
        // advance the tip.
        let get_tip_futs: Vec<_> = (0..4)
            .map(|_| mining.get_tip_request().send().promise)
            .collect();
        let get_tip_results = futures::future::try_join_all(get_tip_futs)
            .await
            .expect("concurrent get_tip should all dispatch via pool");
        for resp in &get_tip_results {
            let results = resp.get().unwrap();
            assert!(results.get_has_result(), "node should have a tip");
            let tip = results.get_result().unwrap();
            assert_eq!(
                tip.get_hash().unwrap().len(),
                32,
                "block hash must be 32 bytes"
            );
            assert!(tip.get_height() >= 0, "height must be non-negative");
        }
    })
    .await;
}

/// Calling the deprecated makeMiningOld2 (@2) should return an error from the
/// server. Cap'n Proto requires sequential ordinals so this placeholder cannot
/// be removed, but the server intentionally rejects it.
#[tokio::test]
#[serial_test::parallel]
async fn make_mining_old2_rejected() {
    with_init_client(|client, _thread| async move {
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

/// isTestChain, isInitialBlockDownload, getTip.
#[tokio::test]
#[serial_test::parallel]
async fn mining_basic_queries() {
    with_mining_client(|_client, thread, mining| async move {
        // isTestChain
        let mut req = mining.is_test_chain_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        let resp = req.send().promise.await.unwrap();
        assert!(resp.get().unwrap().get_result(), "regtest is a test chain");

        // isInitialBlockDownload
        let mut req = mining.is_initial_block_download_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        let resp = req.send().promise.await.unwrap();
        let _ibd: bool = resp.get().unwrap().get_result();

        // getTip
        let mut req = mining.get_tip_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        let resp = req.send().promise.await.unwrap();
        let results = resp.get().unwrap();
        assert!(results.get_has_result(), "node should have a tip");
        let tip = results.get_result().unwrap();
        let tip_hash = tip.get_hash().unwrap();
        assert_eq!(tip_hash.len(), 32, "block hash must be 32 bytes");
        assert!(tip.get_height() >= 0, "height must be non-negative");
    })
    .await;
}

/// waitTipChanged with a short timeout.
#[tokio::test]
// Serialized because this assertion is sensitive to concurrent tip changes.
#[serial_test::serial]
async fn mining_wait_tip_changed() {
    with_mining_client(|_client, thread, mining| async move {
        // Get the current tip first.
        let mut req = mining.get_tip_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        let resp = req.send().promise.await.unwrap();
        let results = resp.get().unwrap();
        let tip = results.get_result().unwrap();
        let tip_hash: Vec<u8> = tip.get_hash().unwrap().to_vec();
        let tip_height: i32 = tip.get_height();

        // Wait with a short timeout; no new block should arrive.
        let mut req = mining.wait_tip_changed_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
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
    with_mining_client(|_client, thread, mining| async move {
        let template = make_block_template(&mining, &thread).await;

        // getBlockHeader
        let mut req = template.get_block_header_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        let resp = req.send().promise.await.unwrap();
        let header = resp.get().unwrap().get_result().unwrap();
        assert_eq!(header.len(), 80, "block header must be 80 bytes");

        // getBlock
        let mut req = template.get_block_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        let resp = req.send().promise.await.unwrap();
        let block = resp.get().unwrap().get_result().unwrap();
        assert!(block.len() > 80, "serialized block must be > 80 bytes");

        // getTxFees
        let mut req = template.get_tx_fees_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        let resp = req.send().promise.await.unwrap();
        let _fees = resp.get().unwrap().get_result().unwrap();

        // getTxSigops
        let mut req = template.get_tx_sigops_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        let resp = req.send().promise.await.unwrap();
        let _sigops = resp.get().unwrap().get_result().unwrap();

        // getCoinbaseTx — inspect every CoinbaseTx field
        let mut req = template.get_coinbase_tx_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        let resp = req.send().promise.await.unwrap();
        let coinbase = resp.get().unwrap().get_result().unwrap();
        let _version: u32 = coinbase.get_version();
        let _sequence: u32 = coinbase.get_sequence();
        let script_sig_prefix = coinbase.get_script_sig_prefix().unwrap();
        assert!(
            !script_sig_prefix.is_empty(),
            "scriptSigPrefix must contain at least the block height"
        );
        let _witness = coinbase.get_witness().unwrap();
        let reward: i64 = coinbase.get_block_reward_remaining();
        assert!(reward > 0 && reward <= mining_capnp::MAX_MONEY);
        let _required_outputs = coinbase.get_required_outputs().unwrap();
        let _lock_time: u32 = coinbase.get_lock_time();

        // getCoinbaseMerklePath
        let mut req = template.get_coinbase_merkle_path_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        let resp = req.send().promise.await.unwrap();
        let _merkle_path = resp.get().unwrap().get_result().unwrap();

        destroy_template(&template, &thread).await;
    })
    .await;
}

/// waitNext (short timeout), interruptWait, submitSolution (garbage), destroy.
#[tokio::test]
// Serialized because submitSolution behavior depends on current chain tip.
#[serial_test::serial]
async fn mining_block_template_lifecycle() {
    with_mining_client(|_client, thread, mining| async move {
        let template = make_block_template(&mining, &thread).await;

        // waitNext — short timeout, no new transactions expected.
        let mut req = template.wait_next_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        {
            let mut opts = req.get().init_options();
            opts.set_timeout(100.0); // 100 ms
            opts.set_fee_threshold(mining_capnp::MAX_MONEY);
        }
        let resp = req.send().promise.await.unwrap();
        let _has_next = resp.get().unwrap().has_result();

        // interruptWait — should not crash.
        template
            .interrupt_wait_request()
            .send()
            .promise
            .await
            .unwrap();

        // submitSolution — garbage coinbase should be rejected.
        // This mutates the template, so we do it right before destroy.
        let mut req = template.submit_solution_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        req.get().set_version(1);
        req.get().set_timestamp(0);
        req.get().set_nonce(0);
        req.get().set_coinbase(&[0u8; 64]);
        let resp = req.send().promise.await.unwrap();
        let submitted = resp.get().unwrap().get_result();
        assert!(!submitted, "garbage solution must not be accepted");

        destroy_template(&template, &thread).await;
    })
    .await;
}

/// checkBlock with a template block payload, and interrupt.
#[tokio::test]
// Serialized because interrupt() can affect other in-flight mining waits.
#[serial_test::serial]
async fn mining_check_block_and_interrupt() {
    with_mining_client(|_client, thread, mining| async move {
        let template = make_block_template(&mining, &thread).await;

        let mut get_block_req = template.get_block_request();
        get_block_req
            .get()
            .get_context()
            .unwrap()
            .set_thread(thread.clone());
        let get_block_resp = get_block_req.send().promise.await.unwrap();
        let block = get_block_resp.get().unwrap().get_result().unwrap().to_vec();

        // checkBlock should either error or return a response.
        let mut req = mining.check_block_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        req.get().set_block(&block);
        {
            let mut opts = req.get().init_options();
            opts.set_check_merkle_root(true);
            opts.set_check_pow(false);
        }
        let result = req.send().promise.await;
        match result {
            Ok(resp) => {
                let results = resp.get().unwrap();
                let _valid: bool = results.get_result();
                let _reason = results.get_reason().unwrap();
                let _debug = results.get_debug().unwrap();
            }
            Err(_) => {
                // Server may reject validation/deserialization.
            }
        }

        destroy_template(&template, &thread).await;

        // interrupt — should not crash.
        mining.interrupt_request().send().promise.await.unwrap();
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
