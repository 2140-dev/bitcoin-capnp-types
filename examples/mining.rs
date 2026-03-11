//! Connect to a running Bitcoin Core node via IPC and exercise the
//! Mining interface: query chain state, create a block template, and
//! inspect its contents.
//!
//! # Prerequisites
//!
//! Start Bitcoin Core in regtest mode with IPC enabled and generate
//! at least 17 blocks so that `createNewBlock` succeeds:
//!
//! ```sh
//! bitcoind -regtest -ipcbind=unix
//! bitcoin-cli -regtest createwallet miner
//! bitcoin-cli -regtest -generate 17
//! ```
//!
//! # Usage
//!
//! ```sh
//! cargo run --example mining
//! ```

use std::path::PathBuf;

use bitcoin_capnp_types::{
    init_capnp::init,
    mining_capnp::{self, block_template, mining},
    proxy_capnp::{thread, thread_map},
};
use capnp_rpc::{RpcSystem, rpc_twoparty_capnp::Side, twoparty::VatNetwork};
use futures::io::BufReader;
use tokio::net::UnixStream;
use tokio::task::LocalSet;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

fn unix_socket_path() -> PathBuf {
    let home_dir = std::env::var("HOME")
        .expect("HOME not set")
        .parse::<PathBuf>()
        .unwrap();
    let bitcoin_dir = if cfg!(target_os = "macos") {
        home_dir
            .join("Library")
            .join("Application Support")
            .join("Bitcoin")
    } else {
        home_dir.join(".bitcoin")
    };
    bitcoin_dir.join("regtest").join("node.sock")
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = unix_socket_path();
    println!("Connecting to {}", path.display());

    // Connect to the Unix socket exposed by Bitcoin Core.
    let stream = UnixStream::connect(&path).await?;
    let (reader, writer) = stream.into_split();
    let rpc_network = VatNetwork::new(
        BufReader::new(reader.compat()),
        futures::io::BufWriter::new(writer.compat_write()),
        Side::Client,
        Default::default(),
    );

    let mut rpc_system = RpcSystem::new(Box::new(rpc_network), None);
    let client: init::Client = rpc_system.bootstrap(Side::Server);

    LocalSet::new()
        .run_until(async move {
            tokio::task::spawn_local(rpc_system);

            // Bootstrap: obtain a thread handle for RPC context.
            let construct_resp = client.construct_request().send().promise.await?;
            let thread_map: thread_map::Client =
                construct_resp.get()?.get_thread_map()?;
            let thread_resp = thread_map.make_thread_request().send().promise.await?;
            let thread: thread::Client = thread_resp.get()?.get_result()?;

            // Create a Mining client.
            let mut req = client.make_mining_request();
            req.get().get_context()?.set_thread(thread.clone());
            let resp = req.send().promise.await?;
            let mining: mining::Client = resp.get()?.get_result()?;

            // ── Chain queries ───────────────────────────────────────

            // isTestChain
            let mut req = mining.is_test_chain_request();
            req.get().get_context()?.set_thread(thread.clone());
            let resp = req.send().promise.await?;
            println!("Test chain: {}", resp.get()?.get_result());

            // isInitialBlockDownload
            let mut req = mining.is_initial_block_download_request();
            req.get().get_context()?.set_thread(thread.clone());
            let resp = req.send().promise.await?;
            println!("Initial block download: {}", resp.get()?.get_result());

            // getTip
            let mut req = mining.get_tip_request();
            req.get().get_context()?.set_thread(thread.clone());
            let resp = req.send().promise.await?;
            let results = resp.get()?;
            if results.get_has_result() {
                let tip = results.get_result()?;
                let tip_hash: Vec<u8> = tip.get_hash()?.to_vec();
                println!(
                    "Tip: height={} hash={}",
                    tip.get_height(),
                    tip_hash
                        .iter()
                        .rev()
                        .map(|b| format!("{b:02x}"))
                        .collect::<String>()
                );
            }

            // ── Block template ──────────────────────────────────────

            // createNewBlock
            let mut req = mining.create_new_block_request();
            req.get().get_context()?.set_thread(thread.clone());
            req.get().set_cooldown(false);
            let resp = req.send().promise.await?;
            let template: block_template::Client = resp.get()?.get_result()?;

            // getBlockHeader — 80-byte raw header.
            let mut req = template.get_block_header_request();
            req.get().get_context()?.set_thread(thread.clone());
            let resp = req.send().promise.await?;
            let header = resp.get()?.get_result()?;
            println!(
                "Block header ({} bytes): {}",
                header.len(),
                header.iter().map(|b| format!("{b:02x}")).collect::<String>()
            );

            // getTxFees
            let mut req = template.get_tx_fees_request();
            req.get().get_context()?.set_thread(thread.clone());
            let resp = req.send().promise.await?;
            let fees = resp.get()?.get_result()?;
            let total_fees: i64 = fees.iter().sum();
            println!("Transactions: {} (total fees: {total_fees} sat)", fees.len());

            // getCoinbaseTx
            let mut req = template.get_coinbase_tx_request();
            req.get().get_context()?.set_thread(thread.clone());
            let resp = req.send().promise.await?;
            let coinbase = resp.get()?.get_result()?;
            println!(
                "Block reward remaining: {} sat (max: {} sat)",
                coinbase.get_block_reward_remaining(),
                mining_capnp::MAX_MONEY
            );

            // getCoinbaseMerklePath
            let mut req = template.get_coinbase_merkle_path_request();
            req.get().get_context()?.set_thread(thread.clone());
            let resp = req.send().promise.await?;
            let merkle_path = resp.get()?.get_result()?;
            println!("Coinbase merkle path: {} hash(es)", merkle_path.len() / 32);

            // Clean up the template.
            let mut req = template.destroy_request();
            req.get().get_context()?.set_thread(thread.clone());
            req.send().promise.await?;
            println!("Template destroyed");

            Ok::<(), Box<dyn std::error::Error>>(())
        })
        .await?;

    Ok(())
}
