use std::{
    io::Read,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use bitcoin_primitives::Transaction as BitcoinTransaction;
use bitcoin_primitives::hex;
use encoding::decode_from_slice;
use serde::de::DeserializeOwned;

fn bitcoin_bin() -> String {
    std::env::var("BITCOIN_BIN").unwrap_or_else(|_| "bitcoin".to_owned())
}

fn bitcoin_rpc_timeout() -> Duration {
    const DEFAULT_TIMEOUT_SECS: u64 = 15;

    std::env::var("BITCOIN_RPC_TIMEOUT_SECS")
        .ok()
        .and_then(|secs| secs.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(DEFAULT_TIMEOUT_SECS))
}

fn bitcoin_rpc(wallet: Option<&str>, args: &[&str]) -> Result<String, String> {
    let owned_args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    bitcoin_rpc_owned(wallet, &owned_args)
}

pub fn bitcoin_rpc_json<T>(wallet: Option<&str>, args: &[&str]) -> Result<T, String>
where
    T: DeserializeOwned,
{
    let output = bitcoin_rpc(wallet, args)?;
    serde_json::from_str(&output).map_err(|e| format!("failed to parse rpc response as JSON: {e}"))
}

pub fn bitcoin_test_wallet() -> String {
    std::env::var("BITCOIN_TEST_WALLET").unwrap_or_else(|_| "ipc-test".to_owned())
}

fn bitcoin_rpc_owned(wallet: Option<&str>, args: &[String]) -> Result<String, String> {
    let mut command = Command::new(bitcoin_bin());
    command.arg("rpc").arg("-chain=regtest").arg("-rpcwait");
    if let Some(wallet) = wallet {
        command.arg(format!("-rpcwallet={wallet}"));
    }
    command.args(args);

    let rendered_args = args.join(" ");
    let timeout = bitcoin_rpc_timeout();
    command.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = command
        .spawn()
        .map_err(|e| format!("failed to execute bitcoin rpc command: {e}"))?;
    let start = Instant::now();

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                if let Some(mut pipe) = child.stdout.take() {
                    pipe.read_to_end(&mut stdout)
                        .map_err(|e| format!("failed to read bitcoin rpc stdout: {e}"))?;
                }
                if let Some(mut pipe) = child.stderr.take() {
                    pipe.read_to_end(&mut stderr)
                        .map_err(|e| format!("failed to read bitcoin rpc stderr: {e}"))?;
                }

                if status.success() {
                    return Ok(String::from_utf8(stdout)
                        .unwrap_or_else(|_| String::new())
                        .trim()
                        .to_owned());
                }

                return Err(format!(
                    "bitcoin rpc command failed ({rendered_args}): {}",
                    String::from_utf8_lossy(&stderr).trim()
                ));
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "bitcoin rpc command timed out after {timeout:?} ({rendered_args})"
                    ));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("failed to poll bitcoin rpc command status: {e}"));
            }
        }
    }
}

pub fn ensure_wallet_loaded(wallet: &str) {
    if bitcoin_rpc(Some(wallet), &["getwalletinfo"]).is_err() {
        // First try loading an existing wallet from disk (common when regtest data
        // directory is re-used), then fall back to creating it.
        if bitcoin_rpc(None, &["loadwallet", wallet]).is_err() {
            let _ = bitcoin_rpc(None, &["createwallet", wallet]);
        }

        bitcoin_rpc(Some(wallet), &["getwalletinfo"]).unwrap_or_else(|e| {
            panic!("wallet {wallet} is not available after load/create attempts: {e}")
        });
    }
}

pub fn ensure_wallet_loaded_and_funded(wallet: &str) {
    ensure_wallet_loaded(wallet);

    // getbalance "*" 1 only counts confirmed spendable funds.
    let balance: f64 = bitcoin_rpc_json(Some(wallet), &["getbalance", "*", "1"])
        .unwrap_or_else(|e| panic!("failed to query wallet balance for {wallet}: {e}"));

    if balance < 1.0 {
        // Mining a single block can mature older coinbase outputs when balance is low.
        mine_blocks_to_new_address(wallet, 1)
            .unwrap_or_else(|e| panic!("failed to mine blocks to wallet {wallet}: {e}"));
    }
}

pub fn mine_blocks_to_new_address(wallet: &str, blocks: u32) -> Result<(), String> {
    let blocks = blocks.to_string();
    let address = bitcoin_rpc(Some(wallet), &["getnewaddress"])?;
    bitcoin_rpc(
        Some(wallet),
        &["generatetoaddress", blocks.as_str(), address.as_str()],
    )?;
    Ok(())
}

pub fn create_mempool_self_transfer(wallet: &str) -> BitcoinTransaction {
    let send_self_transfer = || {
        let address = bitcoin_rpc(Some(wallet), &["getnewaddress"])?;
        let send_args = vec![
            "-named".to_owned(),
            "sendtoaddress".to_owned(),
            format!("address={address}"),
            "amount=0.01".to_owned(),
            "fee_rate=25".to_owned(),
        ];
        bitcoin_rpc_owned(Some(wallet), &send_args)
    };

    let txid_hex = match send_self_transfer() {
        Ok(txid) => txid,
        Err(first_err) => {
            // If the wallet exists but is unfunded or in an unexpected state,
            // try to recover by ensuring funding and retry once.
            ensure_wallet_loaded_and_funded(wallet);
            send_self_transfer().unwrap_or_else(|second_err| {
                panic!(
                    "failed to create self-transfer in {wallet}: initial send failed: {first_err}; retry after funding failed: {second_err}"
                )
            })
        }
    };
    let raw_tx_hex = bitcoin_rpc(None, &["getrawtransaction", txid_hex.as_str()])
        .unwrap_or_else(|e| panic!("failed to fetch raw transaction {txid_hex}: {e}"));
    let raw_tx = hex::decode_to_vec(&raw_tx_hex)
        .unwrap_or_else(|e| panic!("failed to decode raw transaction {txid_hex} from hex: {e}"));
    let tx: BitcoinTransaction = decode_from_slice(&raw_tx)
        .unwrap_or_else(|e| panic!("failed to deserialize raw transaction {txid_hex}: {e}"));
    let txid = tx.compute_txid();
    let txid_display = format!("{txid:x}");
    assert_eq!(
        txid_display, txid_hex,
        "transaction id from raw tx should match RPC txid"
    );
    tx
}
