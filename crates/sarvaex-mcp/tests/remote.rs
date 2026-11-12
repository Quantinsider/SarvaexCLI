//! End-to-end: OAuth 2.1 discovery → DCR → consent → PKCE token → MCP tool
//! calls on behalf of the user, plus refresh rotation, read-only scoping,
//! direct API-key bearer and revocation.

use base64::Engine;
use sarvaex_mcp::store::Store;
use sarvaex_mcp::{McpState, RemoteOptions, build_app};
use sarvaex_mock_gateway::{API_KEY_TRADER, MockGateway, PASSWORD};
use sarvaex_sdk::{Client, ClientConfig};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;

struct Harness {
    http: reqwest::Client,
    base: String,
}

impl Harness {
    async fn start() -> Self {
        let gw = MockGateway::new();
        let gateway_url = gw.spawn().await;
        std::mem::forget(gw);
        let sdk = Client::new(ClientConfig::new(&gateway_url).unwrap()).unwrap();
        let mcp = Arc::new(McpState {
            base: sdk,
            environment_label: "test".into(),
        });
        let store = Arc::new(Store::in_memory([3u8; 32]).unwrap());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let opts = RemoteOptions {
            bind: addr,
            public_url: url::Url::parse(&base).unwrap(),
            state_dir: std::env::temp_dir(),
            allowed_hosts: vec![],
            allow_any_host: false,
            allow_password_login: true,
            allow_api_key_paste: true,
        };
        let app = build_app(mcp, store, opts);
        tokio::spawn(async move {
            axum::serve(listener, app.into_make_service())
                .await
                .unwrap()
        });
        Self {
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            base,
        }
    }

    async fn mcp(&self, token: &str, body: Value) -> (u16, Value) {
        let resp = self
            .http
            .post(format!("{}/mcp", self.base))
            .header("Accept", "application/json, text/event-stream")
            .header("Authorization", format!("Bearer {token}"))
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }

    async fn call_tool(&self, token: &str, name: &str, args: Value) -> (u16, Value) {
        self.mcp(token, json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":name,"arguments":args}})).await
    }

    async fn authorize(&self, client_id: &str, challenge: &str, form: &[(&str, &str)]) -> String {
        let mut params = vec![
            ("response_type", "code"),
            ("client_id", client_id),
            ("redirect_uri", "https://claude.ai/api/mcp/auth_callback"),
            ("scope", "read trade"),
            ("state", "xyz"),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256"),
        ];
        params.extend_from_slice(form);
        let resp = self
            .http
            .post(format!("{}/oauth/authorize", self.base))
            .form(&params)
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status().as_u16(),
            303,
            "{}",
            resp.text().await.unwrap()
        );
        let loc = resp
            .headers()
            .get("location")
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        assert!(loc.starts_with("https://claude.ai/api/mcp/auth_callback?"));
        let u = url::Url::parse(&loc).unwrap();
        assert_eq!(
            u.query_pairs().find(|(k, _)| k == "state").unwrap().1,
            "xyz"
        );
        u.query_pairs()
            .find(|(k, _)| k == "code")
            .unwrap()
            .1
            .into_owned()
    }

    async fn token(&self, form: &[(&str, &str)]) -> (u16, Value) {
        let resp = self
            .http
            .post(format!("{}/oauth/token", self.base))
            .form(form)
            .send()
            .await
            .unwrap();
        (resp.status().as_u16(), resp.json().await.unwrap())
    }
}

fn init_msg() -> Value {
    json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}})
}

fn tool_text(v: &Value) -> Value {
    serde_json::from_str(
        v["result"]["content"][0]["text"]
            .as_str()
            .expect("tool text"),
    )
    .expect("tool json")
}

#[tokio::test]
async fn full_oauth_flow_and_per_user_tool_calls() {
    let h = Harness::start().await;

    // 1. Unauthenticated request → 401 with resource metadata pointer.
    let resp = h
        .http
        .post(format!("{}/mcp", h.base))
        .header("Accept", "application/json, text/event-stream")
        .json(&init_msg())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
    let www = resp
        .headers()
        .get("www-authenticate")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(www.contains("resource_metadata="), "{www}");

    // 2. Discovery.
    let prm: Value = h
        .http
        .get(format!("{}/.well-known/oauth-protected-resource", h.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(prm["resource"], format!("{}/mcp", h.base));
    let asm: Value = h
        .http
        .get(format!("{}/.well-known/oauth-authorization-server", h.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(asm["code_challenge_methods_supported"], json!(["S256"]));

    // 3. Dynamic client registration (public client).
    let resp = h
        .http
        .post(asm["registration_endpoint"].as_str().unwrap())
        .json(&json!({"client_name":"Claude","redirect_uris":["https://claude.ai/api/mcp/auth_callback"],"token_endpoint_auth_method":"none"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);
    let client: Value = resp.json().await.unwrap();
    let client_id = client["client_id"].as_str().unwrap().to_owned();
    assert!(client.get("client_secret").is_none());

    // 4. Consent page renders.
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    let page = h
        .http
        .get(format!("{}/oauth/authorize", h.base))
        .query(&[
            ("response_type", "code"),
            ("client_id", client_id.as_str()),
            ("redirect_uri", "https://claude.ai/api/mcp/auth_callback"),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("state", "xyz"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(page.status().as_u16(), 200);
    assert!(page.text().await.unwrap().contains("Allow trading"));

    // Unknown redirect URI must not redirect.
    let bad = h
        .http
        .get(format!("{}/oauth/authorize", h.base))
        .query(&[
            ("response_type", "code"),
            ("client_id", client_id.as_str()),
            ("redirect_uri", "https://evil.example/cb"),
            ("code_challenge", "x"),
            ("code_challenge_method", "S256"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status().as_u16(), 400);

    // 5. Sign in with password, allow trading.
    let code = h
        .authorize(
            &client_id,
            &challenge,
            &[
                ("mode", "password"),
                ("identifier", "alice_1"),
                ("password", PASSWORD),
                ("allow_trade", "on"),
            ],
        )
        .await;

    // Wrong password re-renders the form with 401 (no redirect).
    let resp = h
        .http
        .post(format!("{}/oauth/authorize", h.base))
        .form(&[
            ("response_type", "code"),
            ("client_id", client_id.as_str()),
            ("redirect_uri", "https://claude.ai/api/mcp/auth_callback"),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("mode", "password"),
            ("identifier", "alice_1"),
            ("password", "wrong"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);

    // 6. Token: wrong verifier fails and consumes the code; fresh code + right verifier works.
    let (status, body) = h
        .token(&[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("client_id", &client_id),
            ("code_verifier", "wrong"),
        ])
        .await;
    assert_eq!(status, 400);
    assert_eq!(body["error"], "invalid_grant");
    let (status, _body) = h
        .token(&[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("client_id", &client_id),
            ("code_verifier", verifier),
        ])
        .await;
    assert_eq!(status, 400, "codes are single-use");
    let code = h
        .authorize(
            &client_id,
            &challenge,
            &[
                ("mode", "password"),
                ("identifier", "alice_1"),
                ("password", PASSWORD),
                ("allow_trade", "on"),
            ],
        )
        .await;
    let (status, tok) = h
        .token(&[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", "https://claude.ai/api/mcp/auth_callback"),
            ("client_id", &client_id),
            ("code_verifier", verifier),
        ])
        .await;
    assert_eq!(status, 200, "{tok}");
    assert_eq!(tok["scope"], "read trade");
    let access = tok["access_token"].as_str().unwrap().to_owned();
    let refresh = tok["refresh_token"].as_str().unwrap().to_owned();

    // 7. MCP calls run as alice.
    let (status, v) = h.mcp(&access, init_msg()).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["result"]["serverInfo"]["name"], "sarvaex");
    let (_, v) = h.call_tool(&access, "get_balance", json!({})).await;
    assert_eq!(tool_text(&v)["raw"]["user_id"], "alice_1");
    let (_, v) = h.call_tool(&access, "place_order", json!({"ticker":"SX-FEDDEC-26OCT-H25","side":"yes","action":"buy","count":1,"price":"0.51"})).await;
    let placed = tool_text(&v);
    assert_eq!(placed["status"], "FILLED", "{placed}");
    assert_eq!(placed["order"]["user_id"], "alice_1");
    let (_, v) = h.call_tool(&access, "place_order", json!({"ticker":"SX-FEDDEC-26OCT-H25","side":"long","action":"buy","count":1,"price":"0.51"})).await;
    assert_eq!(
        v["result"]["isError"], true,
        "side mismatch surfaces as a tool error: {v}"
    );

    // 8. Refresh rotates; the old refresh token dies.
    let (status, t2) = h
        .token(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", &refresh),
            ("client_id", &client_id),
        ])
        .await;
    assert_eq!(status, 200);
    assert_ne!(t2["access_token"], tok["access_token"]);
    let (status, _) = h
        .token(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", &refresh),
            ("client_id", &client_id),
        ])
        .await;
    assert_eq!(status, 400);

    // 9. Read-only consent → `read` scope → trading tools refuse, market data works.
    let code = h
        .authorize(
            &client_id,
            &challenge,
            &[
                ("mode", "password"),
                ("identifier", "alice_1"),
                ("password", PASSWORD),
            ],
        )
        .await;
    let (_, ro) = h
        .token(&[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("client_id", &client_id),
            ("code_verifier", verifier),
        ])
        .await;
    assert_eq!(ro["scope"], "read");
    let ro_access = ro["access_token"].as_str().unwrap();
    let (_, v) = h.call_tool(ro_access, "place_order", json!({"ticker":"SX-FEDDEC-26OCT-H25","side":"yes","action":"buy","count":1,"price":"0.51"})).await;
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("read-only"),
        "{v}"
    );
    let (_, v) = h
        .call_tool(
            ro_access,
            "get_orderbook",
            json!({"ticker":"SX-FEDDEC-26OCT-H25"}),
        )
        .await;
    assert_eq!(tool_text(&v)["best_ask"]["price"], "$0.51");

    // 10. Pasted API key path.
    let code = h
        .authorize(
            &client_id,
            &challenge,
            &[
                ("mode", "apikey"),
                ("api_key", API_KEY_TRADER),
                ("allow_trade", "on"),
            ],
        )
        .await;
    let (status, pk) = h
        .token(&[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("client_id", &client_id),
            ("code_verifier", verifier),
        ])
        .await;
    assert_eq!(status, 200);
    let (_, v) = h
        .call_tool(
            pk["access_token"].as_str().unwrap(),
            "list_positions",
            json!({}),
        )
        .await;
    assert!(tool_text(&v)["positions"].is_array());

    // 11. Direct SarvaEX API key as bearer.
    let (status, v) = h.mcp(API_KEY_TRADER, init_msg()).await;
    assert_eq!(status, 200, "{v}");

    // 12. Revocation kills the connection.
    let t2_access = t2["access_token"].as_str().unwrap();
    let resp = h
        .http
        .post(format!("{}/oauth/revoke", h.base))
        .form(&[("token", t2_access)])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let (status, _) = h.mcp(t2_access, init_msg()).await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn expanded_api_tools_work_with_a_direct_api_key() {
    let h = Harness::start().await;
    let key = API_KEY_TRADER;
    let (status, v) = h.mcp(key, init_msg()).await;
    assert_eq!(status, 200, "{v}");

    // Preflight reads.
    let (_, v) = h.call_tool(key, "get_exchange_status", json!({})).await;
    let st = tool_text(&v);
    assert_eq!(st["mode"], "OPEN");
    assert_eq!(st["new_orders_allowed"], true);
    let (_, v) = h.call_tool(key, "get_fee_schedule", json!({})).await;
    assert_eq!(tool_text(&v)["taker_fee_bps"], 0);

    // Rest an order, amend it in place, then cancel it through the batch tool.
    let (_, v) = h
        .call_tool(
            key,
            "place_order",
            json!({"ticker":"SX-FEDDEC-26OCT-H25","side":"yes","action":"buy","count":3,"price":"0.10"}),
        )
        .await;
    let placed = tool_text(&v);
    assert_eq!(placed["status"], "OPEN", "{placed}");
    let id = placed["order"]["order_id"].as_str().unwrap().to_owned();
    let (_, v) = h
        .call_tool(key, "amend_order", json!({"order_id": id, "price": "0.12"}))
        .await;
    let amended = tool_text(&v);
    assert_eq!(amended["amended_via"], "IN_PLACE", "{amended}");
    assert_eq!(amended["order"]["price_ticks"], 12);
    let (_, v) = h
        .call_tool(key, "cancel_orders", json!({"order_ids": [id]}))
        .await;
    assert_eq!(tool_text(&v)["cancelled"][0], id.as_str());

    // Closing with no position is a tool error carrying the gateway's code.
    let (_, v) = h
        .call_tool(
            key,
            "close_position",
            json!({"ticker":"SX-FEDDEC-26OCT-H25"}),
        )
        .await;
    assert_eq!(v["result"]["isError"], true, "{v}");
    assert!(
        v["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("NO_POSITION"),
        "{v}"
    );

    // Dead man's switch on and off.
    let (_, v) = h
        .call_tool(key, "set_cancel_after", json!({"seconds": 30}))
        .await;
    assert_eq!(tool_text(&v)["armed"], true);
    let (_, v) = h
        .call_tool(key, "set_cancel_after", json!({"seconds": 0}))
        .await;
    assert_eq!(tool_text(&v)["armed"], false);

    // Market data and the activity feed.
    let (_, v) = h
        .call_tool(
            key,
            "get_market_stats",
            json!({"ticker":"SX-FEDDEC-26OCT-H25"}),
        )
        .await;
    assert_eq!(tool_text(&v)["ticker"], "SX-FEDDEC-26OCT-H25");
    let (_, v) = h
        .call_tool(
            key,
            "get_candles",
            json!({"ticker":"SX-FEDDEC-26OCT-H25","interval":"5m"}),
        )
        .await;
    assert_eq!(tool_text(&v)["interval"], "5m");
    let (_, v) = h
        .call_tool(key, "get_account_activity", json!({"types":"order,cash"}))
        .await;
    assert!(!tool_text(&v)["activity"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn host_validation_rejects_unknown_hosts() {
    let h = Harness::start().await;
    let resp = h
        .http
        .post(format!("{}/mcp", h.base))
        .header("Host", "evil.example.com")
        .header("Accept", "application/json, text/event-stream")
        .header("Authorization", format!("Bearer {API_KEY_TRADER}"))
        .json(&init_msg())
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_client_error(), "got {}", resp.status());
}

#[tokio::test]
async fn trigger_orders_work_through_the_place_order_tool() {
    let h = Harness::start().await;
    let key = API_KEY_TRADER;
    let (status, v) = h.mcp(key, init_msg()).await;
    assert_eq!(status, 200, "{v}");

    // Dry run shows what would be sent, with a reduce-only trigger by default.
    let (_, v) = h
        .call_tool(
            key,
            "place_order",
            json!({"ticker":"SXF-USCPIYOY-26SEP","side":"long","action":"sell","count":2,"order_type":"STOP_MARKET","trigger_price":"4.20","dry_run":true}),
        )
        .await;
    let dry = tool_text(&v);
    assert_eq!(dry["dry_run"], true, "{dry}");
    assert_eq!(dry["would_send"]["order_type"], "STOP_MARKET");
    assert_eq!(dry["would_send"]["trigger_price_ticks"], 420);
    assert_eq!(dry["would_send"]["reduce_only"], true);

    // For real: it comes back pending, armed at the trigger price.
    let (_, v) = h
        .call_tool(
            key,
            "place_order",
            json!({"ticker":"SXF-USCPIYOY-26SEP","side":"long","action":"sell","count":2,"order_type":"stop_limit","trigger_price":"4.20","price":"4.15"}),
        )
        .await;
    let placed = tool_text(&v);
    assert_eq!(placed["status"], "TRIGGER_PENDING", "{placed}");
    assert_eq!(placed["order"]["trigger_price_ticks"], 420);
    assert_eq!(placed["order"]["price_ticks"], 415);
    let id = placed["order"]["order_id"].as_str().unwrap().to_owned();

    // It is a working order the agent can cancel by id.
    let (_, v) = h
        .call_tool(key, "cancel_orders", json!({"order_ids": [id]}))
        .await;
    assert_eq!(tool_text(&v)["cancelled"][0], id.as_str());

    // Misuse is a tool error, not a silent plain order: no trigger price, or a binary contract.
    for args in [
        json!({"ticker":"SXF-USCPIYOY-26SEP","side":"long","action":"sell","count":1,"order_type":"STOP_MARKET"}),
        json!({"ticker":"SX-FEDDEC-26OCT-H25","side":"yes","action":"sell","count":1,"order_type":"STOP_MARKET","trigger_price":"0.40"}),
    ] {
        let (_, v) = h.call_tool(key, "place_order", args).await;
        assert_eq!(v["result"]["isError"], true, "{v}");
    }
}
