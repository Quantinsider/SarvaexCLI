//! Black-box tests of the commands for the expanded gateway API, run against the mock gateway.
use sarvaex_mock_gateway::{API_KEY_TRADER, MockGateway};
use std::process::Command;

const BINARY: &str = "SX-FEDDEC-26OCT-H25";

struct Env {
    base: String,
    dir: std::path::PathBuf,
}

async fn setup(name: &str) -> Env {
    let gw = MockGateway::new();
    let base = gw.spawn().await;
    let dir = std::env::temp_dir().join(format!("sarvaex-exp-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::mem::forget(gw);
    Env { base, dir }
}

fn run(env: &Env, args: &[&str], extra_env: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sarvaex"));
    cmd.args(args)
        .env("SARVAEX_BASE_URL", &env.base)
        .env("SARVAEX_API_KEY", API_KEY_TRADER)
        .env("SARVAEX_CONFIG", env.dir.join("config.toml"))
        .env_remove("SARVAEX_READ_ONLY")
        .env_remove("SARVAEX_MAX_ORDER_NOTIONAL_USDC")
        .env_remove("SARVAEX_ALLOWED_TICKERS");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run sarvaex");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn json(s: &str) -> serde_json::Value {
    serde_json::from_str(s).unwrap_or_else(|e| panic!("not json ({e}): {s}"))
}

/// Run a command that must succeed and return its JSON output.
fn ok(env: &Env, args: &[&str]) -> serde_json::Value {
    let (code, out, err) = run(env, args, &[]);
    assert_eq!(code, 0, "{args:?} failed ({code}): {out}{err}");
    json(&out)
}

/// Rest a far-from-market buy and return its order id.
fn rest_order(env: &Env, price: &str) -> String {
    let v = ok(
        env,
        &[
            "orders", "place", BINARY, "--side", "yes", "--action", "buy", "--price", price,
            "--count", "3",
        ],
    );
    assert_eq!(v["order"]["status"], "OPEN", "{v}");
    v["order"]["order_id"].as_str().unwrap().to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exchange_commands_report_status_schedule_time_and_fees() {
    let env = setup("exchange").await;
    let v = ok(&env, &["exchange", "status"]);
    assert_eq!(v["mode"], "OPEN");
    assert_eq!(v["new_orders_allowed"], true);
    let v = ok(&env, &["exchange", "schedule"]);
    assert_eq!(
        v["contracts_closing_next_7d"][0]["ticker"],
        "SX-BTCD-26OCT30H17-T80000"
    );
    assert!(ok(&env, &["exchange", "time"])["epoch_ms"].is_number());
    assert_eq!(ok(&env, &["exchange", "fees"])["taker_fee_bps"], 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn market_data_commands_return_candles_trades_stats_and_books() {
    let env = setup("market").await;
    // Create a trade so the derived endpoints have data.
    let v = ok(
        &env,
        &[
            "orders", "place", BINARY, "--side", "yes", "--action", "buy", "--price", "0.51",
            "--count", "2",
        ],
    );
    assert_eq!(v["order"]["status"], "FILLED");

    let v = ok(&env, &["markets", "candles", BINARY, "--interval", "5m"]);
    assert_eq!(v["interval"], "5m");
    assert_eq!(v["candles"][0]["open"], 51);

    let v = ok(&env, &["markets", "trades", BINARY, "--limit", "5"]);
    assert_eq!(v["trades"][0]["price_ticks"], 51);

    let v = ok(&env, &["markets", "stats", BINARY]);
    assert_eq!(v["last_price_ticks"], 51);
    let v = ok(&env, &["markets", "stats", BINARY, "SX-EMPTYBOOK"]);
    assert_eq!(v["count"], 2);

    let v = ok(&env, &["markets", "books", BINARY, "SX-EMPTYBOOK", "NOPE"]);
    assert_eq!(v["count"], 2);
    assert_eq!(v["errors"][0]["code"], "NOT_FOUND");

    // A bad interval is the gateway's error, surfaced as a failure.
    let (code, _, err) = run(
        &env,
        &["markets", "candles", BINARY, "--interval", "2m"],
        &[],
    );
    assert_ne!(code, 0);
    assert!(err.contains("INVALID_ARGUMENT"), "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn amend_batch_and_bulk_cancel() {
    let env = setup("orders").await;

    // Price-only amend keeps the order's place in the queue.
    let id = rest_order(&env, "0.10");
    let v = ok(&env, &["orders", "amend", &id, "--price", "0.12"]);
    assert_eq!(v["amended_via"], "IN_PLACE");
    assert_eq!(v["order"]["price_ticks"], 12);

    // A count change is replaced (cancel + new order).
    let v = ok(&env, &["orders", "amend", &id, "--count", "5"]);
    assert_eq!(v["amended_via"], "CANCEL_REPLACE");

    // Amend needs something to change.
    let (code, _, err) = run(&env, &["orders", "amend", &id], &[]);
    assert_ne!(code, 0);
    assert!(err.contains("--price"), "{err}");

    // Batch: two good orders, one with a zero price. Exit 4 because one failed.
    let file = env.dir.join("batch.json");
    std::fs::write(
        &file,
        format!(
            r#"[{{"ticker":"{BINARY}","side":"yes","action":"buy","price":"0.05","count":1}},
               {{"ticker":"{BINARY}","side":"yes","action":"buy","price_ticks":6,"count":2,"tif":"gtc"}},
               {{"ticker":"{BINARY}","side":"yes","action":"buy","price_ticks":0,"count":1}}]"#
        ),
    )
    .unwrap();
    let (code, out, err) = run(
        &env,
        &["orders", "batch", file.to_str().unwrap(), "--yes"],
        &[],
    );
    // price_ticks 0 is refused locally before anything is sent.
    assert_ne!(code, 0, "{out}{err}");

    std::fs::write(
        &file,
        format!(
            r#"[{{"ticker":"{BINARY}","side":"yes","action":"buy","price":"0.05","count":1}},
               {{"ticker":"{BINARY}","side":"yes","action":"buy","price_ticks":6,"count":2,"tif":"gtc"}}]"#
        ),
    )
    .unwrap();
    let v = ok(&env, &["orders", "batch", file.to_str().unwrap(), "--yes"]);
    assert_eq!(v["accepted"], 2);
    assert_eq!(v["failed"], 0);
    let ids: Vec<String> = v["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["body"]["order"]["order_id"].as_str().unwrap().to_owned())
        .collect();

    // cancel-batch cancels the named ones and reports unknown ids as failed (exit 1).
    let v = ok(&env, &["orders", "cancel-batch", &ids[0]]);
    assert_eq!(v["cancelled"][0], ids[0].as_str());
    let (code, out, _) = run(&env, &["orders", "cancel-batch", "ord_missing"], &[]);
    assert_eq!(code, 1);
    assert_eq!(json(&out)["failed"][0]["order_id"], "ord_missing");

    // cancel-all sweeps what is left.
    let v = ok(&env, &["orders", "cancel-all", "--yes"]);
    assert!(!v["cancelled"].as_array().unwrap().is_empty(), "{v}");
    let v = ok(&env, &["orders", "list", "--status", "open"]);
    assert_eq!(v["orders"].as_array().map_or(0, Vec::len), 0, "{v}");

    // Batch bounds are checked before anything is sent.
    let big = format!(
        "[{}]",
        vec![
            format!(
                r#"{{"ticker":"{BINARY}","side":"yes","action":"buy","price":"0.05","count":1}}"#
            );
            21
        ]
        .join(",")
    );
    std::fs::write(&file, big).unwrap();
    let (code, _, err) = run(
        &env,
        &["orders", "batch", file.to_str().unwrap(), "--yes"],
        &[],
    );
    assert_ne!(code, 0);
    assert!(err.contains("1 to 20"), "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_position_and_no_position_error() {
    let env = setup("close").await;
    let (code, _, err) = run(&env, &["positions", "close", BINARY, "--yes"], &[]);
    assert_ne!(code, 0);
    assert!(err.contains("NO_POSITION"), "{err}");

    // Buy to open a position, then close it.
    ok(
        &env,
        &[
            "orders", "place", BINARY, "--side", "yes", "--action", "buy", "--price", "0.51",
            "--count", "2",
        ],
    );
    let v = ok(&env, &["positions", "close", BINARY, "--yes"]);
    assert_eq!(v["position_before"], 2);
    assert_eq!(v["closing"]["count"], 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dead_mans_switch_arms_reports_and_disarms() {
    let env = setup("cancelafter").await;
    assert_eq!(
        ok(&env, &["account", "cancel-after", "status"])["armed"],
        false
    );
    let v = ok(&env, &["account", "cancel-after", "set", "60"]);
    assert_eq!(v["armed"], true);
    assert_eq!(v["seconds"], 60);
    assert_eq!(
        ok(&env, &["account", "cancel-after", "status"])["armed"],
        true
    );
    assert_eq!(
        ok(&env, &["account", "cancel-after", "off"])["armed"],
        false
    );

    // Out of range is refused before any request.
    let (code, _, err) = run(&env, &["account", "cancel-after", "set", "99999"], &[]);
    assert_ne!(code, 0);
    assert!(err.contains("3600"), "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_reads_fees_activity_settlements_incentives() {
    let env = setup("account").await;
    assert_eq!(
        ok(&env, &["account", "fees"])["schedule"]["taker_fee_bps"],
        0
    );
    assert!(ok(&env, &["account", "incentives"])["incentives"].is_array());
    assert!(ok(&env, &["account", "settlements"])["settlements"].is_array());

    rest_order(&env, "0.10");
    let v = ok(&env, &["account", "activity", "--limit", "10"]);
    let kinds: Vec<&str> = v["activity"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["type"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"order") && kinds.contains(&"cash"),
        "{kinds:?}"
    );
    let v = ok(&env, &["account", "activity", "--types", "cash"]);
    assert!(
        v["activity"]
            .as_array()
            .unwrap()
            .iter()
            .all(|a| a["type"] == "cash")
    );

    let (code, _, err) = run(&env, &["account", "activity", "--types", "trades"], &[]);
    assert_ne!(code, 0);
    assert!(err.contains("unknown activity type"), "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_key_limits_are_built_from_flags_and_patched() {
    let env = setup("keys").await;
    let v = ok(
        &env,
        &[
            "auth",
            "keys",
            "create",
            "--name",
            "capped",
            "--scopes",
            "markets:read,trading:write",
            "--max-order-count",
            "5",
            "--max-order-notional-usdc",
            "25",
            "--allow-ticker",
            BINARY,
        ],
    );
    assert!(v["key"].as_str().unwrap().starts_with("svx_live_"));
    let key_id = v["key_id"].as_str().unwrap().to_owned();

    let v = ok(
        &env,
        &[
            "auth",
            "keys",
            "limits",
            &key_id,
            "--read-only",
            "--ip-allow",
            "203.0.113.7",
        ],
    );
    assert_eq!(v["limits"]["read_only"], true);
    assert_eq!(v["limits"]["ip_allowlist"][0], "203.0.113.7");

    let v = ok(&env, &["auth", "keys", "limits", &key_id, "--clear"]);
    assert!(v["limits"].is_null());

    // Asking for no change at all is a usage error, not a silent wipe.
    let (code, _, err) = run(&env, &["auth", "keys", "limits", &key_id], &[]);
    assert_ne!(code, 0);
    assert!(err.contains("--clear"), "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_only_mode_blocks_every_new_mutation() {
    let env = setup("readonly").await;
    let id = rest_order(&env, "0.10");
    let guard = [("SARVAEX_READ_ONLY", "true")];
    for args in [
        vec!["orders", "amend", id.as_str(), "--price", "0.11"],
        vec!["orders", "cancel-batch", id.as_str()],
        vec!["positions", "close", BINARY, "--yes"],
        vec!["account", "cancel-after", "set", "30"],
    ] {
        let (code, _, err) = run(&env, &args, &guard);
        assert_eq!(code, 7, "{args:?} should be blocked by the guard: {err}");
    }
    // Reads still work.
    let (code, _, _) = run(&env, &["account", "cancel-after", "status"], &guard);
    assert_eq!(code, 0);
}
