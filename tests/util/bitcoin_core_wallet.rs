use bitcoin_capnp_types::rpc_capnp::rpc;
use bitcoin_primitives::Transaction as BitcoinTransaction;
use bitcoin_primitives::hex;
use encoding::decode_from_slice;
use serde_json::{Value, json};

pub fn bitcoin_datadir() -> Option<String> {
    std::env::var("BITCOIN_DATADIR").ok()
}

/// Call a JSON-RPC method through the IPC `Rpc` interface and return its
/// `result`. With `wallet` set, the request is routed to that wallet the same
/// way `-rpcwallet` does, through the `/wallet/<name>` URI.
pub async fn try_rpc_call(
    rpc: &rpc::Client,
    wallet: Option<&str>,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let mut req = rpc.execute_rpc_request();
    let body = json!({ "jsonrpc": "2.0", "id": "test", "method": method, "params": params });
    req.get().set_request(body.to_string());
    if let Some(wallet) = wallet {
        req.get().set_uri(format!("/wallet/{wallet}"));
    }

    let resp = req
        .send()
        .promise
        .await
        .map_err(|e| format!("rpc {method} failed over IPC: {e}"))?;
    let text = resp
        .get()
        .and_then(|r| r.get_result())
        .map_err(|e| format!("rpc {method} returned a malformed response: {e}"))?
        .to_string()
        .map_err(|e| format!("rpc {method} returned invalid UTF-8: {e}"))?;
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| format!("failed to parse {method} response as JSON: {e}"))?;
    if !v["error"].is_null() {
        return Err(format!("rpc {method} failed: {}", v["error"]));
    }
    Ok(v["result"].clone())
}

/// Call a JSON-RPC method through the IPC `Rpc` interface.
pub async fn rpc_call(rpc: &rpc::Client, method: &str, params: Value) -> Value {
    try_rpc_call(rpc, None, method, params)
        .await
        .unwrap_or_else(|e| panic!("{e}"))
}

pub fn bitcoin_test_wallet() -> String {
    std::env::var("BITCOIN_TEST_WALLET").unwrap_or_else(|_| "ipc-test".to_owned())
}

pub async fn ensure_wallet_loaded(rpc: &rpc::Client, wallet: &str) {
    if try_rpc_call(rpc, Some(wallet), "getwalletinfo", json!([])).await.is_err() {
        // First try loading an existing wallet from disk (common when regtest data
        // directory is re-used), then fall back to creating it.
        if try_rpc_call(rpc, None, "loadwallet", json!([wallet])).await.is_err() {
            let _ = try_rpc_call(rpc, None, "createwallet", json!([wallet])).await;
        }

        try_rpc_call(rpc, Some(wallet), "getwalletinfo", json!([]))
            .await
            .unwrap_or_else(|e| {
                panic!("wallet {wallet} is not available after load/create attempts: {e}")
            });
    }
}

pub async fn ensure_wallet_loaded_and_funded(rpc: &rpc::Client, wallet: &str) {
    ensure_wallet_loaded(rpc, wallet).await;

    // getbalance "*" 1 only counts confirmed spendable funds.
    let balance = try_rpc_call(rpc, Some(wallet), "getbalance", json!(["*", 1]))
        .await
        .unwrap_or_else(|e| panic!("failed to query wallet balance for {wallet}: {e}"))
        .as_f64()
        .expect("getbalance must return a number");

    if balance < 1.0 {
        // Mining a single block can mature older coinbase outputs when balance is low.
        mine_blocks_to_new_address(rpc, wallet, 1)
            .await
            .unwrap_or_else(|e| panic!("failed to mine blocks to wallet {wallet}: {e}"));
    }
}

pub async fn mine_blocks_to_new_address(
    rpc: &rpc::Client,
    wallet: &str,
    blocks: u32,
) -> Result<(), String> {
    let address = try_rpc_call(rpc, Some(wallet), "getnewaddress", json!([])).await?;
    try_rpc_call(
        rpc,
        Some(wallet),
        "generatetoaddress",
        json!([blocks, address]),
    )
    .await?;
    Ok(())
}

pub async fn create_mempool_self_transfer(rpc: &rpc::Client, wallet: &str) -> BitcoinTransaction {
    let send_self_transfer = || async {
        let address = try_rpc_call(rpc, Some(wallet), "getnewaddress", json!([])).await?;
        let txid = try_rpc_call(
            rpc,
            Some(wallet),
            "sendtoaddress",
            json!({ "address": address, "amount": 0.01, "fee_rate": 25 }),
        )
        .await?;
        txid.as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("sendtoaddress returned a non-string txid: {txid}"))
    };

    let txid_hex = match send_self_transfer().await {
        Ok(txid) => txid,
        Err(first_err) => {
            // If the wallet exists but is unfunded or in an unexpected state,
            // try to recover by ensuring funding and retry once.
            ensure_wallet_loaded_and_funded(rpc, wallet).await;
            send_self_transfer().await.unwrap_or_else(|second_err| {
                panic!(
                    "failed to create self-transfer in {wallet}: initial send failed: {first_err}; retry after funding failed: {second_err}"
                )
            })
        }
    };
    let raw_tx_hex = rpc_call(rpc, "getrawtransaction", json!([txid_hex])).await;
    let raw_tx_hex = raw_tx_hex
        .as_str()
        .unwrap_or_else(|| panic!("getrawtransaction {txid_hex} must return a hex string"));
    let raw_tx = hex::decode_to_vec(raw_tx_hex)
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
