use std::{
    future::Future,
    path::{Path, PathBuf},
    sync::Once,
};

use crate::bitcoin_core_wallet_util::{
    bitcoin_rpc_json, bitcoin_test_wallet, ensure_wallet_loaded, mine_blocks_to_new_address,
};
use bitcoin_capnp_types::{
    chain_capnp::chain,
    init_capnp::init,
    mining_capnp::{block_template, mining},
    proxy_capnp::{thread, thread_map},
};
use capnp_rpc::{RpcSystem, rpc_twoparty_capnp::Side, twoparty::VatNetwork};
use futures::io::BufReader;
use serde::Deserialize;
use tokio::net::{UnixStream, unix::OwnedReadHalf};
use tokio::task::LocalSet;
use tokio_util::compat::{Compat, TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

static CHAIN_SETUP: Once = Once::new();

pub fn unix_socket_path() -> PathBuf {
    let home_dir_string = std::env::var("HOME").unwrap();
    let home_dir = home_dir_string.parse::<PathBuf>().unwrap();
    let bitcoin_dir = if cfg!(target_os = "macos") {
        home_dir
            .join("Library")
            .join("Application Support")
            .join("Bitcoin")
    } else {
        home_dir.join(".bitcoin")
    };
    let regtest_dir = bitcoin_dir.join("regtest");
    regtest_dir.join("node.sock")
}

pub fn mempool_tx_count() -> usize {
    let mempool_info: MempoolInfo = bitcoin_rpc_json(None, &["getmempoolinfo"])
        .unwrap_or_else(|e| panic!("failed to query mempool info: {e}"));
    mempool_info.size
}

fn ensure_bootstrap_chain_ready() {
    // `call_once` serializes bootstrap initialization across all tests in this
    // process. Other callers block until this setup completes.
    CHAIN_SETUP.call_once(|| {
        let wallet = bitcoin_test_wallet();
        ensure_chain_height_at_least(101, &wallet);
    });
}

fn ensure_chain_height_at_least(min_height: u32, wallet: &str) {
    ensure_wallet_loaded(wallet);
    let height: u32 = bitcoin_rpc_json(None, &["getblockcount"])
        .unwrap_or_else(|e| panic!("failed to query block height: {e}"));
    if height < min_height {
        mine_blocks_to_new_address(wallet, min_height - height)
            .unwrap_or_else(|e| panic!("failed to reach height {min_height}: {e}"));
    }
}

pub async fn with_init_client<F, Fut>(f: F)
where
    F: FnOnce(init::Client, thread::Client) -> Fut,
    Fut: Future<Output = ()>,
{
    // Per-test wall-clock cap. The happy path of every test in this file
    // completes in a few seconds against a warm regtest node, but bugs in
    // the wait/notification machinery (or a stuck node) can make these
    // futures hang indefinitely -- which then ties up the CI runner until
    // its job-level timeout fires and prints unhelpful output. Bound it
    // here so a misbehaving test fails with a clear message instead.
    const TEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

    let rpc_network = connect_unix_stream(unix_socket_path()).await;
    let rpc_system = RpcSystem::new(Box::new(rpc_network), None);
    let local = LocalSet::new();
    let body = local.run_until(async move {
        let (client, thread) = bootstrap(rpc_system).await;
        f(client, thread).await;
    });
    tokio::time::timeout(TEST_TIMEOUT, body)
        .await
        .unwrap_or_else(|_| panic!("test exceeded {TEST_TIMEOUT:?} wall-clock budget"));
}

pub async fn with_mining_client<F, Fut>(f: F)
where
    F: FnOnce(init::Client, thread::Client, mining::Client) -> Fut,
    Fut: Future<Output = ()>,
{
    with_init_client(|client, thread| async move {
        let mining = make_mining(&client, &thread).await;
        f(client, thread, mining).await;
    })
    .await;
}

pub async fn with_chain_client<F, Fut>(f: F)
where
    F: FnOnce(init::Client, thread::Client, chain::Client) -> Fut,
    Fut: Future<Output = ()>,
{
    with_init_client(|client, thread| async move {
        let chain_client = make_chain(&client, &thread).await;
        f(client, thread, chain_client).await;
    })
    .await;
}

pub async fn connect_unix_stream(
    path: impl AsRef<Path>,
) -> VatNetwork<BufReader<Compat<OwnedReadHalf>>> {
    let path = path.as_ref();
    let mut last_err = None;
    for _ in 0..10 {
        match UnixStream::connect(path).await {
            Ok(stream) => {
                let (reader, writer) = stream.into_split();
                let buf_reader = futures::io::BufReader::new(reader.compat());
                let buf_writer = futures::io::BufWriter::new(writer.compat_write());
                return VatNetwork::new(buf_reader, buf_writer, Side::Client, Default::default());
            }
            Err(e) => {
                last_err = Some(e);
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
    panic!(
        "unix socket connection to {} failed after retries: {}. Is bitcoin running with -ipcbind=unix?",
        path.display(),
        last_err.unwrap()
    );
}

/// Bootstrap an Init client, spawn the RPC system, and create a thread handle.
pub async fn bootstrap(
    mut rpc_system: RpcSystem<capnp_rpc::rpc_twoparty_capnp::Side>,
) -> (init::Client, thread::Client) {
    ensure_bootstrap_chain_ready();

    let client: init::Client = rpc_system.bootstrap(Side::Server);
    tokio::task::spawn_local(rpc_system);
    let create_client_response = client
        .construct_request()
        .send()
        .promise
        .await
        .expect("could not create initial request");
    let thread_map: thread_map::Client = create_client_response
        .get()
        .unwrap()
        .get_thread_map()
        .unwrap();
    let thread_reponse = thread_map
        .make_thread_request()
        .send()
        .promise
        .await
        .unwrap();
    let thread: thread::Client = thread_reponse.get().unwrap().get_result().unwrap();
    (client, thread)
}

/// Obtain a Mining client from an Init client.
pub async fn make_mining(init: &init::Client, thread: &thread::Client) -> mining::Client {
    let mut req = init.make_mining_request();
    req.get().get_context().unwrap().set_thread(thread.clone());
    let resp = req.send().promise.await.unwrap();
    resp.get().unwrap().get_result().unwrap()
}

/// Obtain a Chain client from an Init client.
pub async fn make_chain(init: &init::Client, thread: &thread::Client) -> chain::Client {
    let mut req = init.make_chain_request();
    req.get().get_context().unwrap().set_thread(thread.clone());
    let resp = req.send().promise.await.unwrap();
    resp.get().unwrap().get_result().unwrap()
}

/// Create a new block template with default options and no cooldown.
///
/// The node must have height > 16. At height <= 16 the BIP34 height push
/// is only one byte, which is shorter than the two-byte minimum scriptSig
/// required by consensus (see `CheckTransaction`), causing `createNewBlock`
/// to fail with `bad-cb-length`. `bootstrap()` ensures chain height is at
/// least 101 before tests run, which satisfies this precondition.
pub async fn make_block_template(
    mining: &mining::Client,
    thread: &thread::Client,
) -> block_template::Client {
    let mut req = mining.create_new_block_request();
    req.get().get_context().unwrap().set_thread(thread.clone());
    req.get().set_cooldown(false);
    let resp = req.send().promise.await.unwrap();
    resp.get().unwrap().get_result().unwrap()
}

/// Destroy a block template.
pub async fn destroy_template(template: &block_template::Client, thread: &thread::Client) {
    let mut req = template.destroy_request();
    req.get().get_context().unwrap().set_thread(thread.clone());
    req.send().promise.await.unwrap();
}

#[derive(Deserialize)]
// Intentionally partial: tests currently only need the `size` field from
// `getmempoolinfo`.
struct MempoolInfo {
    size: usize,
}
