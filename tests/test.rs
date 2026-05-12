use std::cell::RefCell;
use std::rc::Rc;

use bitcoin_capnp_types::{chain_capnp::chain_notifications, mining_capnp};

#[path = "util/bitcoin_core.rs"]
mod bitcoin_core_util;
#[path = "util/bitcoin_core_wallet.rs"]
mod bitcoin_core_wallet_util;

use bitcoin_core_util::{
    destroy_template, make_block_template, mempool_tx_count, with_chain_client, with_init_client,
    with_mining_client,
};
use bitcoin_core_wallet_util::{
    bitcoin_test_wallet, create_mempool_self_transfer, ensure_wallet_loaded_and_funded,
    mine_blocks_to_new_address,
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

// -- Chain interface tests ---------------------------------------------------

/// Smoke test the Chain interface bootstrap path and basic queries.
#[tokio::test]
#[serial_test::parallel]
async fn chain_basic_queries() {
    with_chain_client(|_init, thread, chain| async move {
        // getHeight
        let mut req = chain.get_height_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        let resp = req.send().promise.await.unwrap();
        let res = resp.get().unwrap();
        assert!(res.get_has_result(), "node should have a tip height");
        let height: i32 = res.get_result();
        assert!(
            height >= 101,
            "bootstrap helper should ensure height >= 101"
        );

        // getBlockHash at the tip
        let mut req = chain.get_block_hash_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        req.get().set_height(height);
        let resp = req.send().promise.await.unwrap();
        let hash = resp.get().unwrap().get_result().unwrap();
        assert_eq!(hash.len(), 32, "block hash must be 32 bytes");

        // isInitialBlockDownload
        let mut req = chain.is_initial_block_download_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        let resp = req.send().promise.await.unwrap();
        let _ibd: bool = resp.get().unwrap().get_result();

        // havePruned
        let mut req = chain.have_pruned_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        let resp = req.send().promise.await.unwrap();
        assert!(!resp.get().unwrap().get_result(), "regtest is not pruned");
    })
    .await;
}

/// Decode a serialized `CFeeRate` blob (`SERIALIZE_METHODS(CFeeRate, obj)
/// { READWRITE(obj.m_feerate.fee, obj.m_feerate.size); }`, where
/// `m_feerate` is a `FeeFrac { int64_t fee; int32_t size; }`) into
/// satoshis per kilo-vbyte, matching `CFeeRate::GetFeePerK()`.
fn fee_rate_sat_per_kvb(blob: &[u8]) -> i64 {
    assert_eq!(
        blob.len(),
        12,
        "CFeeRate wire format is FeeFrac (int64 LE fee + int32 LE size)"
    );
    let fee = i64::from_le_bytes(blob[0..8].try_into().unwrap());
    let size = i32::from_le_bytes(blob[8..12].try_into().unwrap()) as i64;
    if size == 0 {
        return 0;
    }
    fee.saturating_mul(1000) / size
}

/// Verify `Chain.relayMinFee` returns the node's minimum relay feerate as a
/// serialized `CFeeRate` (default `DEFAULT_MIN_RELAY_TX_FEE = 100`
/// sat/kvB). This is the IPC equivalent of `getnetworkinfo`'s `relayfee`
/// field used by electrs's `Daemon::get_relay_fee`.
#[tokio::test]
#[serial_test::parallel]
async fn chain_relay_min_fee_returns_default() {
    with_chain_client(|_init, thread, chain| async move {
        let mut req = chain.relay_min_fee_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        let resp = req.send().promise.await.unwrap();
        let blob = resp.get().unwrap().get_result().unwrap();
        let sat_per_kvb = fee_rate_sat_per_kvb(blob);
        assert_eq!(
            sat_per_kvb, 100,
            "regtest defaults to DEFAULT_MIN_RELAY_TX_FEE = 100 sat/kvB"
        );
    })
    .await;
}

/// Verify `Chain.estimateSmartFee` returns a (zeroed) `CFeeRate` blob on
/// regtest, where the smart fee estimator has no data to work with. The
/// shape of the response is what matters: callers must be able to decode
/// `result :Data` as a `CFeeRate`.
#[tokio::test]
#[serial_test::parallel]
async fn chain_estimate_smart_fee_returns_decodable_blob() {
    with_chain_client(|_init, thread, chain| async move {
        let mut req = chain.estimate_smart_fee_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        {
            let mut params = req.get();
            params.set_num_blocks(2);
            params.set_conservative(false);
            params.set_want_calc(false);
        }
        let resp = req.send().promise.await.unwrap();
        let blob = resp.get().unwrap().get_result().unwrap();
        let sat_per_kvb = fee_rate_sat_per_kvb(blob);
        // Regtest has no estimator history, so we expect the "no data"
        // sentinel of CFeeRate{} (fee=0, size=0 -> can't divide; the
        // serializer ships zeros and we treat that as "no estimate").
        // We tolerate either 0 or a positive value (in case a future
        // node version returns the floor of the relay fee here).
        assert!(
            sat_per_kvb >= 0,
            "estimateSmartFee should never produce a negative feerate"
        );
    })
    .await;
}

/// Verify findBlock(wantData=true) returns the full serialized block. This is
/// the call electrs uses to fetch raw blocks via IPC instead of P2P.
#[tokio::test]
#[serial_test::parallel]
async fn chain_find_block_returns_data() {
    with_chain_client(|_init, thread, chain| async move {
        // Fetch the genesis block (height 0) via getBlockHash + findBlock.
        let mut req = chain.get_block_hash_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        req.get().set_height(0);
        let resp = req.send().promise.await.unwrap();
        let genesis_hash: Vec<u8> = resp.get().unwrap().get_result().unwrap().to_vec();

        let mut req = chain.find_block_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        req.get().set_hash(&genesis_hash);
        {
            let mut params = req.get().init_block();
            params.set_want_data(true);
            params.set_want_height(true);
            params.set_want_hash(true);
        }
        let resp = req.send().promise.await.unwrap();
        let result = resp.get().unwrap();
        assert!(result.get_result(), "genesis block must be findable");
        let block_result = result.get_block().unwrap();
        let data = block_result.get_data().unwrap();
        assert!(
            data.len() > 80,
            "serialized block (header + txs) must be > 80 bytes, got {}",
            data.len()
        );
        let echoed_hash = block_result.get_hash().unwrap();
        assert_eq!(echoed_hash, genesis_hash.as_slice());
        assert_eq!(block_result.get_height(), 0);
    })
    .await;
}

/// Verify broadcastTransaction succeeds for a transaction that is already in
/// the mempool. The node treats this as a re-announcement and returns OK,
/// which is enough to exercise the request/response framing end-to-end.
///
/// This is the call electrs uses (when configured with the IPC backend) to
/// replace JSON-RPC `sendrawtransaction`.
#[tokio::test]
#[serial_test::serial]
async fn chain_broadcast_transaction() {
    let wallet = bitcoin_test_wallet();
    ensure_wallet_loaded_and_funded(&wallet);
    let tx = create_mempool_self_transfer(&wallet);
    let tx_bytes = encoding::encode_to_vec(&tx);

    with_chain_client(|_init, thread, chain| async move {
        let mut req = chain.broadcast_transaction_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        req.get().set_tx(&tx_bytes);
        // max_tx_fee = 0 means "don't enforce a maximum"; the node will skip
        // the test_accept fee check and re-announce the existing mempool tx.
        req.get().set_max_tx_fee(0);
        // node::TxBroadcast::MEMPOOL_AND_BROADCAST_TO_ALL = 0
        req.get().set_broadcast_method(0);
        let resp = req.send().promise.await.unwrap();
        let r = resp.get().unwrap();
        assert!(
            r.get_result(),
            "broadcastTransaction should succeed (re-announce path); error: {:?}",
            r.get_error().ok().and_then(|e| e.to_str().ok())
        );
    })
    .await;
}

/// Verify findAncestorByHeight returns the expected block hash.
#[tokio::test]
#[serial_test::parallel]
async fn chain_find_ancestor_by_height() {
    with_chain_client(|_init, thread, chain| async move {
        // Look up the tip first.
        let mut req = chain.get_height_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        let resp = req.send().promise.await.unwrap();
        let height: i32 = resp.get().unwrap().get_result();

        let mut req = chain.get_block_hash_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        req.get().set_height(height);
        let resp = req.send().promise.await.unwrap();
        let tip_hash: Vec<u8> = resp.get().unwrap().get_result().unwrap().to_vec();

        // Ask for ancestor at height 50 starting from the tip.
        let mut req = chain.find_ancestor_by_height_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        req.get().set_block_hash(&tip_hash);
        req.get().set_ancestor_height(50);
        {
            let mut params = req.get().init_ancestor();
            params.set_want_hash(true);
            params.set_want_height(true);
        }
        let resp = req.send().promise.await.unwrap();
        let result = resp.get().unwrap();
        assert!(result.get_result(), "ancestor at height 50 must exist");
        let ancestor = result.get_ancestor().unwrap();
        assert_eq!(ancestor.get_height(), 50);
        assert_eq!(ancestor.get_hash().unwrap().len(), 32);
    })
    .await;
}

/// Verify findLocatorFork returns the height of the last common block when
/// passed a CBlockLocator containing only the genesis block hash.
///
/// CBlockLocator wire format (Bitcoin Core src/primitives/block.h):
///   int32 LE version (DUMMY_VERSION = 70016) + CompactSize count + 32*count
///   block hashes ordered tip→genesis.
///
/// This is the call electrs uses to find the fork point between its own
/// header chain and the node's active chain when syncing new headers via IPC
/// instead of P2P `getheaders`.
#[tokio::test]
#[serial_test::parallel]
async fn chain_find_locator_fork() {
    with_chain_client(|_init, thread, chain| async move {
        // Get genesis hash via getBlockHash(0).
        let mut req = chain.get_block_hash_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        req.get().set_height(0);
        let resp = req.send().promise.await.unwrap();
        let genesis_hash: Vec<u8> = resp.get().unwrap().get_result().unwrap().to_vec();
        assert_eq!(genesis_hash.len(), 32);

        // Build CBlockLocator { version=70016, vHave=[genesis] }.
        let mut locator = Vec::with_capacity(4 + 1 + 32);
        locator.extend_from_slice(&70016i32.to_le_bytes());
        locator.push(1u8); // CompactSize for length 1
        locator.extend_from_slice(&genesis_hash);

        let mut req = chain.find_locator_fork_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        req.get().set_locator(&locator);
        let resp = req.send().promise.await.unwrap();
        let r = resp.get().unwrap();
        assert!(
            r.get_has_result(),
            "fork height must be reported for genesis-only locator"
        );
        assert_eq!(
            r.get_result(),
            0,
            "fork height for genesis-only locator must be 0"
        );
    })
    .await;
}

/// Verify waitForNotificationsIfTipChanged returns immediately when the
/// supplied `oldTip` does not match the current tip. This is the cheap
/// no-op case (called when electrs is already aware of the latest tip).
#[tokio::test]
#[serial_test::parallel]
async fn chain_wait_for_notifications_returns_immediately_when_tip_differs() {
    with_chain_client(|_init, thread, chain| async move {
        let mut req = chain.wait_for_notifications_if_tip_changed_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        // All-zeros hash is guaranteed not to be the current tip.
        req.get().set_old_tip(&[0u8; 32]);
        let fut = req.send().promise;
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), fut).await;
        assert!(
            result.is_ok(),
            "waitForNotificationsIfTipChanged should return ~immediately when oldTip != current tip"
        );
        result.unwrap().unwrap();
    })
    .await;
}

/// Verify waitForNotificationsIfTipChanged blocks until a new block arrives
/// when called with the current tip hash, then returns. This is the call
/// electrs uses (when configured with the IPC backend) to replace its P2P
/// `inv`-watching loop for new-block notifications.
#[tokio::test]
#[serial_test::serial]
async fn chain_wait_for_notifications_unblocks_on_new_block() {
    let wallet = bitcoin_test_wallet();
    ensure_wallet_loaded_and_funded(&wallet);

    with_chain_client(|_init, thread, chain| async move {
        // Snapshot current tip.
        let mut req = chain.get_height_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        let resp = req.send().promise.await.unwrap();
        let height: i32 = resp.get().unwrap().get_result();

        let mut req = chain.get_block_hash_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        req.get().set_height(height);
        let resp = req.send().promise.await.unwrap();
        let tip_hash: Vec<u8> = resp.get().unwrap().get_result().unwrap().to_vec();

        // Kick off the wait *before* mining the block so we exercise the
        // blocking path rather than the immediate-return path.
        let mut req = chain.wait_for_notifications_if_tip_changed_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        req.get().set_old_tip(&tip_hash);
        let wait_fut = req.send().promise;

        // Mine one block from a separate task so we don't deadlock the
        // current_thread runtime on the bitcoin-cli subprocess.
        let mine_wallet = wallet.clone();
        let mine_handle = tokio::task::spawn_blocking(move || {
            // Small delay so the wait is established before the block lands.
            std::thread::sleep(std::time::Duration::from_millis(200));
            mine_blocks_to_new_address(&mine_wallet, 1)
                .expect("failed to mine block to wake the wait");
        });

        let result = tokio::time::timeout(std::time::Duration::from_secs(15), wait_fut).await;
        assert!(
            result.is_ok(),
            "waitForNotificationsIfTipChanged should unblock after a new block is mined"
        );
        result.unwrap().unwrap();
        mine_handle.await.unwrap();
    })
    .await;
}

/// Minimal `ChainNotifications::Server` used by the notification tests
/// below. Records every `transactionAddedToMempool` /
/// `transactionRemovedFromMempool` event in interior-mutable buffers so the
/// test body can poll them. Other notification methods accept and ignore
/// the call (they're delivered too, but the tests don't assert on them).
#[derive(Default)]
struct RecordingNotifications {
    added: Rc<RefCell<Vec<Vec<u8>>>>,
    #[allow(clippy::type_complexity)]
    removed: Rc<RefCell<Vec<(Vec<u8>, i32)>>>,
}

impl chain_notifications::Server for RecordingNotifications {
    fn destroy(
        self: Rc<Self>,
        _: chain_notifications::DestroyParams,
        _: chain_notifications::DestroyResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        std::future::ready(Ok(()))
    }

    fn transaction_added_to_mempool(
        self: Rc<Self>,
        params: chain_notifications::TransactionAddedToMempoolParams,
        _: chain_notifications::TransactionAddedToMempoolResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let added = self.added.clone();
        async move {
            let p = params.get()?;
            let tx = p.get_tx()?.to_vec();
            added.borrow_mut().push(tx);
            Ok(())
        }
    }

    fn transaction_removed_from_mempool(
        self: Rc<Self>,
        params: chain_notifications::TransactionRemovedFromMempoolParams,
        _: chain_notifications::TransactionRemovedFromMempoolResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let removed = self.removed.clone();
        async move {
            let p = params.get()?;
            let tx = p.get_tx()?.to_vec();
            let reason = p.get_reason();
            removed.borrow_mut().push((tx, reason));
            Ok(())
        }
    }

    fn block_connected(
        self: Rc<Self>,
        _: chain_notifications::BlockConnectedParams,
        _: chain_notifications::BlockConnectedResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        std::future::ready(Ok(()))
    }

    fn block_disconnected(
        self: Rc<Self>,
        _: chain_notifications::BlockDisconnectedParams,
        _: chain_notifications::BlockDisconnectedResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        std::future::ready(Ok(()))
    }

    fn updated_block_tip(
        self: Rc<Self>,
        _: chain_notifications::UpdatedBlockTipParams,
        _: chain_notifications::UpdatedBlockTipResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        std::future::ready(Ok(()))
    }

    fn chain_state_flushed(
        self: Rc<Self>,
        _: chain_notifications::ChainStateFlushedParams,
        _: chain_notifications::ChainStateFlushedResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        std::future::ready(Ok(()))
    }
}

/// Verify `Chain.handleNotifications` delivers `transactionAddedToMempool`
/// callbacks when a new transaction enters the node's mempool. This is the
/// path electrs (when configured with the IPC backend) needs to use to
/// retire its periodic `getrawmempool` polling.
#[tokio::test]
#[serial_test::serial]
async fn chain_handle_notifications_delivers_mempool_added() {
    let wallet = bitcoin_test_wallet();
    ensure_wallet_loaded_and_funded(&wallet);

    with_chain_client(|_init, thread, chain| async move {
        let recorder = Rc::new(RecordingNotifications::default());
        let recorder_for_assert = recorder.clone();
        let notifications: chain_notifications::Client =
            capnp_rpc::new_client_from_rc(recorder.clone());

        // Register the handler. We must keep the returned Handler client
        // alive for the duration of the subscription.
        let mut req = chain.handle_notifications_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        req.get().set_notifications(notifications.clone());
        let resp = req.send().promise.await.unwrap();
        let handler = resp.get().unwrap().get_result().unwrap();

        // Drain any prior mempool state so the subsequent self-transfer is
        // unambiguously the trigger.
        recorder_for_assert.added.borrow_mut().clear();

        // Inject a fresh transaction into the node's mempool from a
        // blocking task (bitcoin-cli is sync) and wait for the notification
        // to arrive.
        let inject_wallet = wallet.clone();
        let inject = tokio::task::spawn_blocking(move || {
            create_mempool_self_transfer(&inject_wallet);
        });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            if !recorder_for_assert.added.borrow().is_empty() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!(
                    "transactionAddedToMempool was not delivered within 15s; recorded={}",
                    recorder_for_assert.added.borrow().len()
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        inject.await.unwrap();

        let mut req = handler.destroy_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        req.send().promise.await.unwrap();
    })
    .await;
}

/// Verify `Chain.requestMempoolTransactions` replays the current mempool
/// contents through the supplied `ChainNotifications` handler. This is the
/// "give me what's already in the mempool" primer electrs needs at startup
/// before switching to the live notification stream.
#[tokio::test]
#[serial_test::serial]
async fn chain_request_mempool_transactions_replays_current_mempool() {
    let wallet = bitcoin_test_wallet();
    ensure_wallet_loaded_and_funded(&wallet);

    // Seed the mempool with a single transaction synchronously (outside
    // the runtime) so it's already there when we ask for the snapshot.
    let _seed = create_mempool_self_transfer(&wallet);
    assert!(
        mempool_tx_count() >= 1,
        "expected at least one tx in the node's mempool before request"
    );

    with_chain_client(|_init, thread, chain| async move {
        let recorder = Rc::new(RecordingNotifications::default());
        let notifications: chain_notifications::Client =
            capnp_rpc::new_client_from_rc(recorder.clone());

        let mut req = chain.request_mempool_transactions_request();
        req.get().get_context().unwrap().set_thread(thread.clone());
        req.get().set_notifications(notifications);
        let resp = tokio::time::timeout(std::time::Duration::from_secs(15), req.send().promise)
            .await
            .expect("requestMempoolTransactions timed out");
        resp.expect("requestMempoolTransactions failed");

        // The replay is fire-and-forget on the server side; give the
        // delivered callbacks a brief moment to land on our LocalSet
        // before asserting.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if !recorder.added.borrow().is_empty() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!(
                    "requestMempoolTransactions delivered no transactionAddedToMempool callbacks"
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await;
}
