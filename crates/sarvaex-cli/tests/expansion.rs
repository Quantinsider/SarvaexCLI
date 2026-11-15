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

const FUTURES: &str = "SXF-USCPIYOY-26SEP";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_and_take_profit_orders_place_list_get_and_cancel() {
    let env = setup("triggers").await;

    // A stop-market arms at a price and reduces a position by default.
    let v = ok(
        &env,
        &[
            "orders",
            "place",
            FUTURES,
            "--side",
            "long",
            "--action",
            "sell",
            "--count",
            "2",
            "--type",
            "stop-market",
            "--trigger-price",
            "4.20",
        ],
    );
    assert_eq!(v["order"]["status"], "TRIGGER_PENDING", "{v}");
    assert_eq!(v["order"]["trigger_price_ticks"], 420);
    assert_eq!(v["order"]["reduce_only"], true);
    let stop_id = v["order"]["order_id"].as_str().unwrap().to_owned();
    assert!(stop_id.starts_with("trg_"));
    assert!(
        v["summary"].as_str().unwrap().contains("arms at 420"),
        "{v}"
    );

    // --allow-open lets a trigger open or grow a position.
    let v = ok(
        &env,
        &[
            "orders",
            "place",
            FUTURES,
            "--side",
            "long",
            "--action",
            "buy",
            "--count",
            "1",
            "--type",
            "take-profit",
            "--trigger-price-ticks",
            "450",
            "--allow-open",
        ],
    );
    assert_eq!(v["order"]["reduce_only"], false);

    // A *-limit type also takes a limit price.
    let v = ok(
        &env,
        &[
            "orders",
            "place",
            FUTURES,
            "--side",
            "long",
            "--action",
            "sell",
            "--count",
            "1",
            "--type",
            "stop-limit",
            "--trigger-price",
            "4.10",
            "--price",
            "4.05",
        ],
    );
    assert_eq!(v["order"]["price_ticks"], 405);
    assert_eq!(v["order"]["trigger_price_ticks"], 410);

    // The request that would be sent is visible without sending it.
    let v = ok(
        &env,
        &[
            "orders",
            "place",
            FUTURES,
            "--side",
            "short",
            "--action",
            "buy",
            "--count",
            "1",
            "--type",
            "take-profit-limit",
            "--trigger-price",
            "3.90",
            "--price",
            "3.95",
            "--dry-run",
        ],
    );
    assert_eq!(v["would_send"]["order_type"], "TAKE_PROFIT_LIMIT");
    assert_eq!(v["would_send"]["trigger_price_ticks"], 390);
    assert_eq!(v["would_send"]["price_ticks"], 395);

    // Listing and getting.
    let v = ok(&env, &["orders", "list", "--status", "trigger-pending"]);
    assert_eq!(v["count"], 3, "{v}");
    assert!(
        v["orders"]
            .as_array()
            .unwrap()
            .iter()
            .all(|o| o["trigger_price_ticks"].is_number())
    );
    let v = ok(&env, &["orders", "get", &stop_id]);
    assert_eq!(v["status"], "TRIGGER_PENDING");

    // Cancel one; cancel-all takes the rest.
    let v = ok(&env, &["orders", "cancel", &stop_id]);
    assert_eq!(v["order"]["status"], "TRIGGER_CANCELLED", "{v}");
    let v = ok(&env, &["orders", "list", "--status", "trigger-pending"]);
    assert_eq!(v["count"], 2);
    let v = ok(&env, &["orders", "cancel-all", "--yes"]);
    assert_eq!(v["cancelled"].as_array().unwrap().len(), 2, "{v}");
    let v = ok(&env, &["orders", "list", "--status", "trigger-pending"]);
    assert_eq!(v["count"], 0);
    let v = ok(&env, &["orders", "list", "--status", "trigger-cancelled"]);
    assert_eq!(v["count"], 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn trigger_orders_are_refused_locally_when_misused() {
    let env = setup("triggerbad").await;
    let fails = |args: &[&str], needle: &str| {
        let (code, _, err) = run(&env, args, &[]);
        assert_ne!(code, 0, "{args:?} should fail");
        assert!(
            err.contains(needle),
            "{args:?}: expected '{needle}' in {err}"
        );
    };
    // Binary contracts have no triggers.
    fails(
        &[
            "orders",
            "place",
            BINARY,
            "--side",
            "yes",
            "--action",
            "sell",
            "--count",
            "1",
            "--type",
            "stop-market",
            "--trigger-price",
            "0.40",
        ],
        "futures",
    );
    // A trigger type needs a trigger price.
    fails(
        &[
            "orders",
            "place",
            FUTURES,
            "--side",
            "long",
            "--action",
            "sell",
            "--count",
            "1",
            "--type",
            "stop-market",
        ],
        "--trigger-price",
    );
    // A trigger price needs a trigger type.
    fails(
        &[
            "orders",
            "place",
            FUTURES,
            "--side",
            "long",
            "--action",
            "sell",
            "--count",
            "1",
            "--price",
            "4.20",
            "--trigger-price",
            "4.00",
        ],
        "--type",
    );
    // A market-style trigger takes no limit price; a limit-style one needs it.
    fails(
        &[
            "orders",
            "place",
            FUTURES,
            "--side",
            "long",
            "--action",
            "sell",
            "--count",
            "1",
            "--type",
            "stop-market",
            "--trigger-price",
            "4.00",
            "--price",
            "3.90",
        ],
        "takes no --price",
    );
    fails(
        &[
            "orders",
            "place",
            FUTURES,
            "--side",
            "long",
            "--action",
            "sell",
            "--count",
            "1",
            "--type",
            "stop-limit",
            "--trigger-price",
            "4.00",
        ],
        "--price",
    );
    // --market is a different kind of order.
    fails(
        &[
            "orders",
            "place",
            FUTURES,
            "--side",
            "long",
            "--action",
            "sell",
            "--count",
            "1",
            "--type",
            "take-profit",
            "--trigger-price",
            "4.00",
            "--market",
        ],
        "--market",
    );
    // Read-only guards block them like any order.
    let (code, _, _) = run(
        &env,
        &[
            "orders",
            "place",
            FUTURES,
            "--side",
            "long",
            "--action",
            "sell",
            "--count",
            "1",
            "--type",
            "stop-market",
            "--trigger-price",
            "4.00",
        ],
        &[("SARVAEX_READ_ONLY", "true")],
    );
    assert_eq!(code, 7);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_limits_can_be_shown_and_only_lowered() {
    let env = setup("limits").await;
    let v = ok(&env, &["account", "limits"]);
    let start = v["max_order_size_micro_usdc"].as_i64().unwrap();
    assert_eq!(start, 10_000_000_000);

    // Lowering in USDC; the answer lists what changed and the new limits.
    let v = ok(
        &env,
        &["account", "limits", "--max-order-size", "5000", "--yes"],
    );
    assert_eq!(v["limits"]["max_order_size_micro_usdc"], 5_000_000_000_i64);
    assert_eq!(v["changed"][0]["from"], start);
    let v = ok(
        &env,
        &[
            "account",
            "limits",
            "--position-limit",
            &format!("{BINARY}=10"),
            "--yes",
        ],
    );
    assert_eq!(v["limits"]["per_contract_position_limit"][BINARY], 10);

    // A higher value is refused with the gateway's code and exit status, and nothing moves.
    let (code, _, err) = run(
        &env,
        &["account", "limits", "--max-order-size", "6000", "--yes"],
        &[],
    );
    assert_ne!(code, 0);
    assert!(err.contains("LIMIT_CANNOT_BE_RAISED"), "{err}");
    assert_eq!(
        ok(&env, &["account", "limits"])["max_order_size_micro_usdc"],
        5_000_000_000_i64
    );

    // Bad input is caught before anything is sent.
    for (args, needle) in [
        (
            vec!["account", "limits", "--position-limit", "nonsense", "--yes"],
            "TICKER=CONTRACTS",
        ),
        (
            vec![
                "account",
                "limits",
                "--position-limit",
                "SX-A=zero",
                "--yes",
            ],
            "whole number",
        ),
        (
            vec!["account", "limits", "--max-order-size", "0", "--yes"],
            "positive",
        ),
    ] {
        let (code, _, err) = run(&env, &args, &[]);
        assert_ne!(code, 0, "{args:?}");
        assert!(err.contains(needle), "{args:?}: {err}");
    }
    // Read-only mode blocks lowering (it is a change) but not looking.
    let guard = [("SARVAEX_READ_ONLY", "true")];
    let (code, _, _) = run(
        &env,
        &["account", "limits", "--max-order-size", "100", "--yes"],
        &guard,
    );
    assert_eq!(code, 7);
    let (code, _, _) = run(&env, &["account", "limits"], &guard);
    assert_eq!(code, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn portfolio_notifications_and_rfq_list() {
    let env = setup("phased").await;
    let v = ok(&env, &["account", "portfolio"]);
    assert!(v["account"]["equity_micro_usdc"].is_number());
    assert!(v["working"]["open_orders"].is_number());

    // Lowering a limit leaves a notification; read state round-trips.
    ok(
        &env,
        &["account", "limits", "--daily-loss-limit", "1000", "--yes"],
    );
    let v = ok(&env, &["account", "notifications", "list"]);
    assert_eq!(v["unread_count"], 1);
    assert_eq!(v["notifications"][0]["kind"], "LIMITS_TIGHTENED");
    let id = v["notifications"][0]["notification_id"]
        .as_i64()
        .unwrap()
        .to_string();
    let v = ok(&env, &["account", "notifications", "read", &id]);
    assert_eq!(v["marked"], 1);
    assert_eq!(
        ok(&env, &["account", "notifications", "list", "--unread"])["count"],
        0
    );
    let (code, _, err) = run(&env, &["account", "notifications", "read"], &[]);
    assert_ne!(code, 0);
    assert!(err.contains("--all"), "{err}");
    assert_eq!(
        ok(&env, &["account", "notifications", "read", "--all"])["unread_count"],
        0
    );

    // RFQ listing: mine by default, filters, and a clear error for a bad role.
    assert_eq!(
        ok(&env, &["rfq", "list"])["rfqs"].as_array().unwrap().len(),
        2
    );
    assert_eq!(
        ok(&env, &["rfq", "list", "--status", "open"])["rfqs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        ok(&env, &["rfq", "list", "--role", "maker"])["rfqs"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    let (code, _, err) = run(&env, &["rfq", "list", "--role", "everyone"], &[]);
    assert_ne!(code, 0);
    assert!(err.contains("role must be"), "{err}");
}
