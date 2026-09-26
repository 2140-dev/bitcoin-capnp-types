use std::{
    future::Future,
    path::{Path, PathBuf},
    sync::Once,
};

use crate::util::bitcoin_core_wallet::{
    bitcoin_datadir, bitcoin_rpc_json, bitcoin_test_wallet, ensure_wallet_loaded,
    mine_blocks_to_new_address,
};
use bitcoin_capnp_types::{
    init_capnp::init,
    mining_capnp::{block_template, mining},
    proxy_capnp::thread_map,
    rpc_capnp::rpc,
};
use capnp_rpc::{RpcSystem, rpc_twoparty_capnp::Side, twoparty::VatNetwork};
use futures::io::BufReader;
use serde::Deserialize;
use tokio::net::{UnixStream, unix::OwnedReadHalf};
use tokio::task::LocalSet;
use tokio_util::compat::{Compat, TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

static CHAIN_SETUP: Once = Once::new();

pub fn unix_socket_path() -> PathBuf {
    let bitcoin_dir = match bitcoin_datadir() {
        Some(datadir) => datadir.parse::<PathBuf>().unwrap(),
        None => {
            let home_dir_string = std::env::var("HOME").unwrap();
            let home_dir = home_dir_string.parse::<PathBuf>().unwrap();
            if cfg!(target_os = "macos") {
                home_dir
                    .join("Library")
                    .join("Application Support")
                    .join("Bitcoin")
            } else {
                home_dir.join(".bitcoin")
            }
        }
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
    F: FnOnce(init::Client) -> Fut,
    Fut: Future<Output = ()>,
{
    let rpc_network = connect_unix_stream(unix_socket_path()).await;
    let rpc_system = RpcSystem::new(Box::new(rpc_network), None);
    LocalSet::new()
        .run_until(async move {
            let client = bootstrap(rpc_system).await;
            f(client).await;
        })
        .await;
}

pub async fn with_mining_client<F, Fut>(f: F)
where
    F: FnOnce(init::Client, mining::Client) -> Fut,
    Fut: Future<Output = ()>,
{
    with_init_client(|client| async move {
        let mining = make_mining(&client).await;
        f(client, mining).await;
    })
    .await;
}

pub async fn with_rpc_client<F, Fut>(f: F)
where
    F: FnOnce(init::Client, rpc::Client) -> Fut,
    Fut: Future<Output = ()>,
{
    with_init_client(|client| async move {
        let rpc = make_rpc(&client).await;
        f(client, rpc).await;
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

/// Bootstrap an Init client, spawn the RPC system, and pre-allocate a server
/// thread pool. Requests sent without `context.thread` set will be dispatched
/// through this pool via a shared work queue.
pub async fn bootstrap(
    mut rpc_system: RpcSystem<capnp_rpc::rpc_twoparty_capnp::Side>,
) -> init::Client {
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
    let mut pool_req = thread_map.make_pool_request();
    pool_req.get().set_count(4);
    pool_req.send().promise.await.unwrap();
    client
}

/// Obtain a Mining client from an Init client.
pub async fn make_mining(init: &init::Client) -> mining::Client {
    let resp = init.make_mining_request().send().promise.await.unwrap();
    resp.get().unwrap().get_result().unwrap()
}

/// Obtain a Rpc client from an Init client.
pub async fn make_rpc(init: &init::Client) -> rpc::Client {
    let resp = init.make_rpc_request().send().promise.await.unwrap();
    resp.get().unwrap().get_result().unwrap()
}

/// Create a new block template with default options and no cooldown.
///
/// The node must have height > 16. At height <= 16 the BIP34 height push
/// is only one byte, which is shorter than the two-byte minimum scriptSig
/// required by consensus (see `CheckTransaction`), causing `createNewBlock`
/// to fail with `bad-cb-length`. `bootstrap()` ensures chain height is at
/// least 101 before tests run, which satisfies this precondition.
pub async fn make_block_template(mining: &mining::Client) -> block_template::Client {
    let mut req = mining.create_new_block_request();
    req.get().set_cooldown(false);
    let resp = req.send().promise.await.unwrap();
    resp.get().unwrap().get_result().unwrap()
}

/// Destroy a block template.
pub async fn destroy_template(template: &block_template::Client) {
    template.destroy_request().send().promise.await.unwrap();
}

#[derive(Deserialize)]
// Intentionally partial: tests currently only need the `size` field from
// `getmempoolinfo`.
struct MempoolInfo {
    size: usize,
}
