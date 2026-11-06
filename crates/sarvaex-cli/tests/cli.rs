//! Black-box tests of the `sarvaex` binary against the mock gateway.
use sarvaex_mock_gateway::{API_KEY_TRADER, MockGateway};
use std::process::Command;

struct Env {
    base: String,
    config: std::path::PathBuf,
}

async fn setup() -> Env {
    let gw = MockGateway::new();
    let base = gw.spawn().await;
    let dir = std::env::temp_dir().join(format!("sarvaex-cli-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Keep the gateway alive for the test's duration by leaking it into a task.
    std::mem::forget(gw);
    Env {
        base,
        config: dir.join("config.toml"),
    }
}

fn run(env: &Env, args: &[&str], extra_env: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sarvaex"));
    cmd.args(args)
        .env("SARVAEX_BASE_URL", &env.base)
        .env("SARVAEX_API_KEY", API_KEY_TRADER)
        .env("SARVAEX_CONFIG", &env.config)
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn piped_output_is_json_and_exit_codes_are_stable() {
    let env = setup().await;

    let (code, out, _) = run(&env, &["markets", "list", "--compact"], &[]);
    assert_eq!(code, 0);
    assert_eq!(json(&out)["count"], 4);

    let (code, out, _) = run(&env, &["markets", "book", "SX-FEDDEC-26OCT-H25"], &[]);
    assert_eq!(code, 0);
    assert_eq!(json(&out)["best_ask"]["price"], "$0.51");

    // filled
    let (code, out, _) = run(
        &env,
        &[
            "orders",
            "place",
            "SX-FEDDEC-26OCT-H25",
            "--side",
            "yes",
            "--action",
            "buy",
            "--price",
            "0.51",
            "--count",
            "2",
        ],
        &[],
    );
    assert_eq!(code, 0, "{out}");
    let v = json(&out);
    assert_eq!(v["order"]["status"], "FILLED");
    assert_eq!(v["display"]["avg_fill_price"], "$0.51");

    // futures display units
    let (code, out, _) = run(
        &env,
        &[
            "orders",
            "place",
            "SXF-USCPIYOY-26SEP",
            "--side",
            "long",
            "--action",
            "buy",
            "--price",
            "4.31",
            "--count",
            "1",
        ],
        &[],
    );
    assert_eq!(code, 0, "{out}");
    assert_eq!(json(&out)["order"]["price_ticks"], 431);

    // server-side rejection → 4
    let (code, out, _) = run(
        &env,
        &[
            "orders",
            "place",
            "SX-FEDDEC-26OCT-H25",
            "--side",
            "yes",
            "--action",
            "buy",
            "--price",
            "0.51",
            "--count",
            "777",
        ],
        &[],
    );
    assert_eq!(code, 4);
    assert_eq!(json(&out)["reject_code"], "POSITION_LIMIT_EXCEEDED");

    // validation → 2
    let (code, _, err) = run(
        &env,
        &[
            "orders",
            "place",
            "SX-FEDDEC-26OCT-H25",
            "--side",
            "long",
            "--action",
            "buy",
            "--price",
            "0.51",
            "--count",
            "1",
        ],
        &[],
    );
    assert_eq!(code, 2);
    assert_eq!(json(&err)["error"]["code"], "VALIDATION");

    // ambiguous binary price → 2
    let (code, _, err) = run(
        &env,
        &[
            "orders",
            "place",
            "SX-FEDDEC-26OCT-H25",
            "--side",
            "yes",
            "--action",
            "buy",
            "--price",
            "51",
            "--count",
            "1",
        ],
        &[],
    );
    assert_eq!(code, 2);
    assert!(err.contains("ambiguous"));

    // guard → 7
    let (code, _, err) = run(
        &env,
        &[
            "orders",
            "place",
            "SX-FEDDEC-26OCT-H25",
            "--side",
            "yes",
            "--action",
            "buy",
            "--price",
            "0.51",
            "--count",
            "1",
        ],
        &[("SARVAEX_READ_ONLY", "1")],
    );
    assert_eq!(code, 7);
    assert_eq!(json(&err)["error"]["code"], "GUARD");

    // auth → 3
    let (code, _, err) = run(
        &env,
        &["account", "balance"],
        &[("SARVAEX_API_KEY", "svx_live_bogus")],
    );
    assert_eq!(code, 3);
    assert_eq!(json(&err)["error"]["code"], "UNAUTHENTICATED");

    // api error (MARKET_NOT_AVAILABLE) → 1 with stable envelope
    let (code, _, err) = run(
        &env,
        &[
            "orders",
            "place",
            "SX-EMPTYBOOK",
            "--side",
            "yes",
            "--action",
            "buy",
            "--market",
            "--count",
            "1",
        ],
        &[],
    );
    assert_eq!(code, 1);
    assert_eq!(json(&err)["error"]["code"], "MARKET_NOT_AVAILABLE");

    // dry run sends nothing
    let (code, out, _) = run(
        &env,
        &[
            "orders",
            "place",
            "SX-FEDDEC-26OCT-H25",
            "--side",
            "yes",
            "--action",
            "buy",
            "--price",
            "0.51",
            "--count",
            "1",
            "--dry-run",
        ],
        &[],
    );
    assert_eq!(code, 0);
    assert_eq!(json(&out)["dry_run"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn login_stores_key_in_profile_and_logout_revokes() {
    let env = setup().await;
    let (code, out, err) = run(
        &env,
        &["auth", "login", "--identifier", "alice_1", "--compact"],
        &[
            ("SARVAEX_API_KEY", ""),
            ("SARVAEX_PASSWORD", sarvaex_mock_gateway::PASSWORD),
        ],
    );
    assert_eq!(code, 0, "{out}{err}");
    let v = json(&out);
    assert_eq!(v["user_id"], "alice_1");
    assert!(
        v["scopes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s == "trading:write")
    );
    let cfg = std::fs::read_to_string(&env.config).unwrap();
    assert!(cfg.contains("api_key = \"svx_live_"));

    // Stored key works without SARVAEX_API_KEY.
    let (code, out, _) = run(&env, &["auth", "whoami"], &[("SARVAEX_API_KEY", "")]);
    assert_eq!(code, 0);
    assert_eq!(json(&out)["user_id"], "alice_1");

    let (code, out, _) = run(
        &env,
        &["auth", "logout", "--revoke"],
        &[("SARVAEX_API_KEY", "")],
    );
    assert_eq!(code, 0);
    assert_eq!(json(&out)["revoked"], true);
    let (code, _, _) = run(&env, &["account", "balance"], &[("SARVAEX_API_KEY", "")]);
    assert_eq!(code, 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn docs_and_config_commands_need_no_network() {
    let env = Env {
        base: "http://127.0.0.1:1".into(),
        config: std::env::temp_dir().join("sarvaex-cli-docs-test.toml"),
    };
    let (code, out, _) = run(&env, &["docs", "skill"], &[]);
    assert_eq!(code, 0);
    assert!(out.contains("# Trading on SarvaEX"));
    let dir = std::env::temp_dir().join(format!("sarvaex-docs-install-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (code, _, _) = run(
        &env,
        &["docs", "install", "--dir", dir.to_str().unwrap()],
        &[],
    );
    assert_eq!(code, 0);
    assert!(dir.join(".claude/skills/sarvaex/SKILL.md").exists());
    assert!(
        std::fs::read_to_string(dir.join("AGENTS.md"))
            .unwrap()
            .contains("SarvaEX")
    );
    let (code, out, _) = run(&env, &["mcp", "config", "claude-desktop"], &[]);
    assert_eq!(code, 0);
    assert_eq!(json(&out)["mcpServers"]["sarvaex"]["args"][1], "stdio");
}
