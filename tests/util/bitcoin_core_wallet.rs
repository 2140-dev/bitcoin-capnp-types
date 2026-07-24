use std::process::Command;

use bitcoin_primitives::Transaction as BitcoinTransaction;
use bitcoin_primitives::hex;
use encoding::decode_from_slice;
use serde::de::DeserializeOwned;

fn bitcoin_bin() -> String {
    std::env::var("BITCOIN_BIN").unwrap_or_else(|_| "bitcoin".to_owned())
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

fn bitcoin_rpc_owned_json<T>(wallet: Option<&str>, args: &[String]) -> Result<T, String>
where
    T: DeserializeOwned,
{
    let output = bitcoin_rpc_owned(wallet, args)?;
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

    let output = command
        .output()
        .map_err(|e| format!("failed to execute bitcoin rpc command: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8(output.stdout)
            .unwrap_or_else(|_| String::new())
            .trim()
            .to_owned())
    } else {
        Err(format!(
            "bitcoin rpc command failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
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
    let tx = transaction_from_hex(&raw_tx_hex);
    let txid = tx.compute_txid();
    let txid_display = format!("{txid:x}");
    assert_eq!(
        txid_display, txid_hex,
        "transaction id from raw tx should match RPC txid"
    );
    tx
}

pub fn create_unbroadcast_self_transfer(wallet: &str) -> BitcoinTransaction {
    ensure_wallet_loaded_and_funded(wallet);

    let address = bitcoin_rpc(Some(wallet), &["getnewaddress"])
        .unwrap_or_else(|e| panic!("failed to get wallet address for {wallet}: {e}"));
    let outputs = format!("{{\"{address}\":0.01}}");
    let raw_tx = bitcoin_rpc_owned(
        Some(wallet),
        &[
            "-named".to_owned(),
            "createrawtransaction".to_owned(),
            "inputs=[]".to_owned(),
            format!("outputs={outputs}"),
        ],
    )
    .unwrap_or_else(|e| panic!("failed to create raw transaction for {wallet}: {e}"));
    let funded: FundRawTransaction = bitcoin_rpc_owned_json(
        Some(wallet),
        &[
            "-named".to_owned(),
            "fundrawtransaction".to_owned(),
            format!("hexstring={raw_tx}"),
            "options={\"fee_rate\":25}".to_owned(),
        ],
    )
    .unwrap_or_else(|e| panic!("failed to fund raw transaction for {wallet}: {e}"));
    let signed: SignRawTransaction = bitcoin_rpc_owned_json(
        Some(wallet),
        &[
            "-named".to_owned(),
            "signrawtransactionwithwallet".to_owned(),
            format!("hexstring={}", funded.hex),
        ],
    )
    .unwrap_or_else(|e| panic!("failed to sign raw transaction for {wallet}: {e}"));
    assert!(signed.complete, "wallet should fully sign self-transfer");
    transaction_from_hex(&signed.hex)
}

fn transaction_from_hex(tx_hex: &str) -> BitcoinTransaction {
    let raw_tx = hex::decode_to_vec(tx_hex)
        .unwrap_or_else(|e| panic!("failed to decode raw transaction {tx_hex} from hex: {e}"));
    decode_from_slice(&raw_tx)
        .unwrap_or_else(|e| panic!("failed to deserialize raw transaction {tx_hex}: {e}"))
}

#[derive(serde::Deserialize)]
struct FundRawTransaction {
    hex: String,
}

#[derive(serde::Deserialize)]
struct SignRawTransaction {
    hex: String,
    complete: bool,
}
