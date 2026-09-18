use bitcoin_capnp_types::{mining_capnp, proxy_capnp::thread};

mod util;

use util::bitcoin_core::{
    destroy_template, make_block_template, mempool_tx_count, with_init_client, with_mining_client,
};
use util::bitcoin_core_wallet::{
    bitcoin_test_wallet, create_mempool_self_transfer, ensure_wallet_loaded_and_funded,
};
use util::block::{block_solution, block_with_pow};

async fn get_template_block(
    template: &mining_capnp::block_template::Client,
    thread: &thread::Client,
) -> Vec<u8> {
    let mut req = template.get_block_request();
    req.get().get_context().unwrap().set_thread(thread.clone());
    let resp = req.send().promise.await.unwrap();
    resp.get().unwrap().get_result().unwrap().to_vec()
}

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
        let _ibd = resp
            .get()
            .expect("isInitialBlockDownload response should decode")
            .get_result();

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
        let _fees = resp
            .get()
            .expect("getTxFees response should decode")
            .get_result()
            .expect("getTxFees response should contain fees");

        // getTxSigops
        let mut req = template.get_tx_sigops_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        let resp = req.send().promise.await.unwrap();
        let _sigops = resp
            .get()
            .expect("getTxSigops response should decode")
            .get_result()
            .expect("getTxSigops response should contain sigops");

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
        let mut req = template.get_coinbase_merkle_path_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        let resp = req.send().promise.await.unwrap();
        let _merkle_path = resp
            .get()
            .expect("getCoinbaseMerklePath response should decode")
            .get_result()
            .expect("getCoinbaseMerklePath response should contain a merkle path");

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

/// submitSolution with a solved template block should be accepted.
#[tokio::test]
#[serial_test::serial]
async fn mining_block_template_submit_solution_resolved_and_duplicate() {
    with_mining_client(|_client, thread, mining| async move {
        let template = make_block_template(&mining, &thread).await;

        let block = get_template_block(&template, &thread).await;
        let block = block_with_pow(&block, true);
        let solution = block_solution(&block);

        let mut req = template.submit_solution_request();
        {
            let mut params = req.get();
            params.set_version(solution.version);
            params.set_timestamp(solution.timestamp);
            params.set_nonce(solution.nonce);
            params.set_coinbase(&solution.coinbase);
            params.get_context().unwrap().set_thread(thread.clone());
        }
        let resp = req.send().promise.await.unwrap();
        assert!(
            resp.get().unwrap().get_result(),
            "solved template solution must be accepted"
        );

        // A duplicate block currently returns true. bitcoin/bitcoin#34672 may
        // change this to false, and this coverage should catch that change.
        let mut req = template.submit_solution_request();
        {
            let mut params = req.get();
            params.set_version(solution.version);
            params.set_timestamp(solution.timestamp);
            params.set_nonce(solution.nonce);
            params.set_coinbase(&solution.coinbase);
            params.get_context().unwrap().set_thread(thread.clone());
        }
        let resp = req.send().promise.await.unwrap();
        assert!(
            resp.get().unwrap().get_result(),
            "duplicate template solution currently returns true"
        );

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

        let block = get_template_block(&template, &thread).await;

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

        destroy_template(&template, &thread).await;

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
