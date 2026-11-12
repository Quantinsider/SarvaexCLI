//! SDK behaviour against the mock gateway.
use sarvaex_mock_gateway::{API_KEY_READONLY, API_KEY_TRADER, MockGateway};
use sarvaex_sdk::{
    Action, Client, ClientConfig, Credential, Guards, MarketFilter, NewOrder, Side, Tif,
};

async fn client(base: &str, key: &str, guards: Guards) -> Client {
    Client::new(
        ClientConfig::new(base)
            .unwrap()
            .with_credential(Credential::ApiKey(key.into()))
            .with_guards(guards),
    )
    .unwrap()
}

#[tokio::test]
async fn market_data_and_pagination() {
    let gw = MockGateway::new();
    let base = gw.spawn().await;
    let c = Client::new(ClientConfig::new(&base).unwrap()).unwrap(); // no credential: public data works
    let page = c
        .list_markets(&MarketFilter {
            limit: Some(2),
            ..MarketFilter::open()
        })
        .await
        .unwrap();
    assert_eq!(page.items.len(), 2);
    assert!(page.has_more());
    let all = c.list_all_markets(&MarketFilter::open(), 10).await.unwrap();
    assert_eq!(all.len(), 4, "closed contract excluded");
    let book = c.orderbook("SX-FEDDEC-26OCT-H25", Some(5)).await.unwrap();
    assert_eq!(book.best_bid().unwrap().price_ticks, 49);
    assert_eq!(book.best_ask().unwrap().price_ticks, 51);
    let err = c.balance().await.unwrap_err();
    assert!(
        err.is_unauthenticated(),
        "private endpoint without credential fails fast: {err}"
    );
}

#[tokio::test]
async fn order_lifecycle() {
    let gw = MockGateway::new();
    let base = gw.spawn().await;
    let c = client(&base, API_KEY_TRADER, Guards::default()).await;

    // Crossing limit fills.
    let resp = c
        .place_order(
            &NewOrder::limit("SX-FEDDEC-26OCT-H25", Side::Yes, Action::Buy, 51, 10),
            None,
        )
        .await
        .unwrap();
    assert!(!resp.is_rejected());
    assert_eq!(resp.order.status, "FILLED");
    assert_eq!(resp.fills.len(), 1);

    // Resting order, then cancel, then cancelling again is a rejection (ORDER_TERMINAL), not an Err.
    let resting = c
        .place_order(
            &NewOrder::limit("SX-FEDDEC-26OCT-H25", Side::Yes, Action::Buy, 40, 5),
            None,
        )
        .await
        .unwrap();
    assert_eq!(resting.order.status, "OPEN");
    let cancelled = c.cancel_order(&resting.order.order_id, None).await.unwrap();
    assert_eq!(cancelled.order.status, "CANCELLED");
    let again = c.cancel_order(&resting.order.order_id, None).await.unwrap();
    assert!(again.is_rejected());
    assert_eq!(again.reject_code, "ORDER_TERMINAL");

    // IOC that does not cross is CANCELLED with nothing filled.
    let ioc = c
        .place_order(
            &NewOrder::limit("SX-FEDDEC-26OCT-H25", Side::Yes, Action::Buy, 40, 5)
                .with_tif(Tif::Ioc),
            None,
        )
        .await
        .unwrap();
    assert_eq!(ioc.order.status, "CANCELLED");
    assert_eq!(ioc.order.filled_count, 0);

    // Positions reflect fills.
    let pos = c.positions(false, None, None).await.unwrap();
    assert_eq!(pos.items.len(), 1);
    assert_eq!(pos.items[0].net_qty, 10);
}

#[tokio::test]
async fn server_rejection_is_ok_with_reject_code() {
    let gw = MockGateway::new();
    let base = gw.spawn().await;
    let c = client(&base, API_KEY_TRADER, Guards::default()).await;
    let resp = c
        .place_order(
            &NewOrder::limit("SX-FEDDEC-26OCT-H25", Side::Yes, Action::Buy, 51, 777),
            None,
        )
        .await
        .unwrap();
    assert!(resp.is_rejected());
    assert_eq!(resp.reject_code, "POSITION_LIMIT_EXCEEDED");
    assert!(
        resp.summary()
            .starts_with("REJECTED (POSITION_LIMIT_EXCEEDED)")
    );
}

#[tokio::test]
async fn local_validation_catches_bad_orders_before_sending() {
    let gw = MockGateway::new();
    let base = gw.spawn().await;
    let c = client(&base, API_KEY_TRADER, Guards::default()).await;
    let calls_before = gw.calls().len();

    let e = c
        .place_order(
            &NewOrder::limit("SX-FEDDEC-26OCT-H25", Side::Long, Action::Buy, 51, 1),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(e.code(), "VALIDATION");
    assert!(e.to_string().contains("YES or NO"));

    let e = c
        .place_order(
            &NewOrder::limit("SX-CLOSED", Side::Yes, Action::Buy, 51, 1),
            None,
        )
        .await
        .unwrap_err();
    assert!(e.to_string().contains("CLOSED"));

    let e = c
        .place_order(
            &NewOrder::limit("SX-FEDDEC-26OCT-H25", Side::Yes, Action::Buy, 150, 1),
            None,
        )
        .await
        .unwrap_err();
    assert!(e.to_string().contains("above the contract maximum"));

    let e = c
        .place_order(
            &NewOrder::limit("SX-FEDDEC-26OCT-H25", Side::Yes, Action::Buy, 51, 5000),
            None,
        )
        .await
        .unwrap_err();
    assert!(e.to_string().contains("max_order_size"));

    // Only GET /v1/markets/{ticker} lookups happened; no POST reached the gateway.
    assert!(
        gw.calls()[calls_before..]
            .iter()
            .all(|c| c.starts_with("GET ")),
        "{:?}",
        gw.calls()
    );
}

#[tokio::test]
async fn market_order_without_liquidity_is_an_api_error() {
    let gw = MockGateway::new();
    let base = gw.spawn().await;
    let c = client(&base, API_KEY_TRADER, Guards::default()).await;
    let e = c
        .place_order(
            &NewOrder::market("SX-EMPTYBOOK", Side::Yes, Action::Buy, 1),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(e.code(), "MARKET_NOT_AVAILABLE");
    assert_eq!(e.status(), Some(400));
    // A market order where liquidity exists fills.
    let ok = c
        .place_order(
            &NewOrder::market("SX-FEDDEC-26OCT-H25", Side::No, Action::Buy, 3),
            None,
        )
        .await
        .unwrap();
    assert_eq!(ok.order.status, "FILLED");
    assert_eq!(
        ok.order.avg_fill_price_ticks, 51,
        "NO fills at 100 - best bid"
    );
}

#[tokio::test]
async fn retries_rate_limit_and_in_progress_and_replays_idempotently() {
    let gw = MockGateway::new();
    let base = gw.spawn().await;
    let c = client(&base, API_KEY_TRADER, Guards::default()).await;
    // First call 429 + Retry-After: 1, second succeeds.
    let m = c.get_market("SX-RATELIMIT").await.unwrap();
    assert_eq!(m.ticker, "SX-FEDDEC-26OCT-H25");
    // 409 OPERATION_IN_PROGRESS then success with the same key.
    let r = c
        .place_order(
            &NewOrder::limit("SX-FEDDEC-26OCT-H25", Side::Yes, Action::Buy, 51, 1),
            Some("inprogress-x"),
        )
        .await
        .unwrap();
    assert_eq!(r.order.status, "FILLED");
    // Same idempotency key + same body replays the stored order.
    let a = c
        .place_order(
            &NewOrder::limit("SX-FEDDEC-26OCT-H25", Side::Yes, Action::Buy, 51, 1)
                .with_client_order_id("k1"),
            Some("order-k1"),
        )
        .await
        .unwrap();
    let b = c
        .place_order(
            &NewOrder::limit("SX-FEDDEC-26OCT-H25", Side::Yes, Action::Buy, 51, 1)
                .with_client_order_id("k1"),
            Some("order-k1"),
        )
        .await
        .unwrap();
    assert_eq!(a.order.order_id, b.order.order_id);
    // Same key, different body → 409 IDEMPOTENCY_KEY_REUSED (not retryable).
    let e = c
        .place_order(
            &NewOrder::limit("SX-FEDDEC-26OCT-H25", Side::Yes, Action::Buy, 51, 2)
                .with_client_order_id("k1"),
            Some("order-k1"),
        )
        .await
        .unwrap_err();
    assert_eq!(e.code(), "IDEMPOTENCY_KEY_REUSED");
}

#[tokio::test]
async fn guards_block_before_network() {
    let gw = MockGateway::new();
    let base = gw.spawn().await;
    let ro = client(
        &base,
        API_KEY_TRADER,
        Guards {
            read_only: true,
            ..Default::default()
        },
    )
    .await;
    let e = ro
        .place_order(
            &NewOrder::limit("SX-FEDDEC-26OCT-H25", Side::Yes, Action::Buy, 51, 1),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(e.code(), "GUARD");
    assert!(ro.cancel_all_orders(None).await.is_err());

    let capped = client(
        &base,
        API_KEY_TRADER,
        Guards {
            max_order_notional_micro: Some(2_000_000),
            ..Default::default()
        },
    )
    .await;
    let e = capped
        .place_order(
            &NewOrder::limit("SX-FEDDEC-26OCT-H25", Side::Yes, Action::Buy, 51, 10),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(e.code(), "GUARD");
    assert!(
        capped
            .place_order(
                &NewOrder::limit("SX-FEDDEC-26OCT-H25", Side::Yes, Action::Buy, 51, 3),
                None
            )
            .await
            .is_ok()
    );

    let allow = client(
        &base,
        API_KEY_TRADER,
        Guards {
            allowed_tickers: Some(vec!["SXF-USCPIYOY-26SEP".into()]),
            ..Default::default()
        },
    )
    .await;
    let e = allow
        .place_order(
            &NewOrder::limit("SX-FEDDEC-26OCT-H25", Side::Yes, Action::Buy, 51, 1),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(e.code(), "GUARD");
    assert!(
        gw.calls().iter().filter(|c| c.starts_with("POST")).count() == 1,
        "only the allowed order was sent"
    );
}

#[tokio::test]
async fn scopes_are_enforced_by_gateway() {
    let gw = MockGateway::new();
    let base = gw.spawn().await;
    let c = client(&base, API_KEY_READONLY, Guards::default()).await;
    assert!(c.balance().await.is_ok());
    let e = c
        .place_order(
            &NewOrder::limit("SX-FEDDEC-26OCT-H25", Side::Yes, Action::Buy, 51, 1),
            None,
        )
        .await
        .unwrap_err();
    assert!(e.is_forbidden());
    assert_eq!(e.code(), "PERMISSION_DENIED");
}

#[tokio::test]
async fn login_and_api_key_lifecycle() {
    let gw = MockGateway::new();
    let base = gw.spawn().await;
    let anon = Client::new(ClientConfig::new(&base).unwrap()).unwrap();
    assert!(anon.login("alice_1", "wrong").await.is_err());
    let login = anon
        .login("alice_1", sarvaex_mock_gateway::PASSWORD)
        .await
        .unwrap();
    let session = anon.with_swapped_credential(Credential::Bearer(login.token));
    let created = session
        .create_api_key("test", &sarvaex_sdk::ALL_SCOPES, None)
        .await
        .unwrap();
    assert!(created.key.starts_with("svx_live_"));
    let keyed = anon.with_swapped_credential(Credential::ApiKey(created.key.clone()));
    assert_eq!(keyed.profile().await.unwrap().user_id, "alice_1");
    keyed.revoke_api_key(&created.key_id).await.unwrap();
    assert!(keyed.profile().await.unwrap_err().is_unauthenticated());
}

#[tokio::test]
async fn cancel_all_only_touches_working_orders() {
    let gw = MockGateway::new();
    let base = gw.spawn().await;
    let c = client(&base, API_KEY_TRADER, Guards::default()).await;
    for px in [40, 41, 42] {
        c.place_order(
            &NewOrder::limit("SX-FEDDEC-26OCT-H25", Side::Yes, Action::Buy, px, 1),
            None,
        )
        .await
        .unwrap();
    }
    c.place_order(
        &NewOrder::limit("SXF-USCPIYOY-26SEP", Side::Long, Action::Buy, 400, 1),
        None,
    )
    .await
    .unwrap();
    let (cancelled, failed) = c
        .cancel_all_orders(Some("SX-FEDDEC-26OCT-H25"))
        .await
        .unwrap();
    assert_eq!(cancelled.len(), 3);
    assert!(failed.is_empty());
    let (cancelled, _) = c.cancel_all_orders(None).await.unwrap();
    assert_eq!(cancelled.len(), 1, "only the futures order was left");
}

#[tokio::test]
async fn websocket_stream_applies_book_and_detects_gaps() {
    use sarvaex_sdk::WsEvent;
    use sarvaex_sdk::ws::{BookUpdate, LocalBook, WsConnection};
    let gw = MockGateway::new();
    let base = gw.spawn().await;
    let c = client(&base, API_KEY_TRADER, Guards::default()).await;
    let mut conn = WsConnection::connect(c.ws_url(), c.credential())
        .await
        .unwrap();
    conn.subscribe_market("SX-FEDDEC-26OCT-H25").await.unwrap();
    conn.subscribe_private("SX-FEDDEC-26OCT-H25").await.unwrap();
    let mut book = LocalBook::new("SX-FEDDEC-26OCT-H25");
    let mut saw = Vec::new();
    // The mock sends exactly 8 frames for market + private subscriptions; bound the wait.
    for _ in 0..8 {
        let next = tokio::time::timeout(std::time::Duration::from_secs(5), conn.next_event())
            .await
            .expect("ws frame within 5s");
        let Some(Ok((event, _raw))) = next else { break };
        match &event {
            WsEvent::Connected { .. } => saw.push("connected"),
            WsEvent::Subscribed { .. } => saw.push("subscribed"),
            WsEvent::MarketBookSnapshot { .. } => {
                assert_eq!(book.apply(&event), BookUpdate::Applied);
                assert_eq!(book.best_bid(), Some((49, 120)));
                saw.push("snapshot");
            }
            WsEvent::MarketBookDelta { book_seq, .. } if *book_seq == 8201 => {
                assert_eq!(book.apply(&event), BookUpdate::Applied);
                assert_eq!(book.best_bid(), Some((49, 100)));
                saw.push("delta");
            }
            WsEvent::MarketBookDelta { .. } => {
                assert!(matches!(
                    book.apply(&event),
                    BookUpdate::Gap {
                        expected: 8202,
                        got: 8204
                    }
                ));
                assert!(book.needs_resync());
                saw.push("gap");
            }
            WsEvent::MarketTrade { price_ticks, .. } => {
                assert_eq!(*price_ticks, 51);
                saw.push("trade");
            }
            WsEvent::PrivateFill { order_id, .. } => {
                assert_eq!(order_id, "ord-x");
                saw.push("private_fill");
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(
        saw,
        [
            "connected",
            "subscribed",
            "snapshot",
            "delta",
            "trade",
            "gap",
            "subscribed",
            "private_fill"
        ]
    );
    conn.close().await;
}

#[tokio::test]
async fn expanded_api_methods_follow_the_documented_shapes() {
    let gw = MockGateway::new();
    let base = gw.spawn().await;
    let c = client(&base, API_KEY_TRADER, Guards::default()).await;

    assert_eq!(c.exchange_status().await.unwrap()["mode"], "OPEN");
    assert_eq!(c.fee_schedule().await.unwrap()["taker_fee_bps"], 0);
    assert!(c.server_time().await.unwrap()["epoch_ms"].is_number());

    // Amend needs a change; a price-only amend stays in place; a count change is replaced.
    let order = NewOrder::limit("SX-FEDDEC-26OCT-H25", Side::Yes, Action::Buy, 10, 3);
    let placed = c.place_order(&order, None).await.unwrap();
    let id = placed.order.order_id.clone();
    assert!(c.amend_order(&id, None, None, true, None).await.is_err());
    let v = c
        .amend_order(&id, Some(12), None, true, None)
        .await
        .unwrap();
    assert_eq!(v["amended_via"], "IN_PLACE");
    let v = c.amend_order(&id, None, Some(5), true, None).await.unwrap();
    assert_eq!(v["amended_via"], "CANCEL_REPLACE");

    // Batch bounds are enforced before any request is sent.
    assert!(c.batch_orders(&[], None).await.is_err());
    let many: Vec<NewOrder> = (0..21).map(|_| order.clone()).collect();
    assert!(c.batch_orders(&many, None).await.is_err());
    let two = vec![order.clone(), order.clone()];
    let v = c.batch_orders(&two, None).await.unwrap();
    assert_eq!(v["accepted"], 2);

    // Dead man's switch bounds and round trip.
    assert!(c.set_cancel_after(3601, None).await.is_err());
    assert_eq!(c.set_cancel_after(45, None).await.unwrap()["armed"], true);
    assert_eq!(c.cancel_after_status().await.unwrap()["seconds"], 45);
    assert_eq!(c.set_cancel_after(0, None).await.unwrap()["armed"], false);

    // Cancelling in bulk uses the single-request endpoint when the gateway has it.
    let (cancelled, failed) = c.cancel_all_orders(None).await.unwrap();
    assert!(!cancelled.is_empty() && failed.is_empty());
    assert!(gw.calls().iter().any(|p| p == "POST /v1/orders/cancel-all"));
}

#[tokio::test]
async fn cancel_all_falls_back_to_one_by_one_on_an_older_gateway() {
    use axum::routing::get;
    // A gateway without the expansion: no cancel-all route (404), orders listing works.
    let app = axum::Router::new().route(
        "/v1/orders",
        get(|| async { axum::Json(serde_json::json!({"orders": [], "next_cursor": ""})) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let c = client(&base, API_KEY_TRADER, Guards::default()).await;
    let (cancelled, failed) = c.cancel_all_orders(None).await.unwrap();
    assert!(cancelled.is_empty() && failed.is_empty());
}

#[tokio::test]
async fn read_only_guard_blocks_the_new_mutations() {
    let gw = MockGateway::new();
    let base = gw.spawn().await;
    let c = client(
        &base,
        API_KEY_TRADER,
        Guards {
            read_only: true,
            ..Guards::default()
        },
    )
    .await;
    assert!(
        c.amend_order("ord_1", Some(5), None, true, None)
            .await
            .unwrap_err()
            .to_string()
            .contains("read-only")
    );
    assert!(c.cancel_batch(&["ord_1".to_owned()], None).await.is_err());
    assert!(c.cancel_all_server(None, None).await.is_err());
    assert!(
        c.close_position("SX-FEDDEC-26OCT-H25", None, None, None)
            .await
            .is_err()
    );
    assert!(c.set_cancel_after(10, None).await.is_err());
    assert!(c.set_api_key_limits("kid-1", None).await.is_err());
    // Reads are unaffected.
    assert!(c.cancel_after_status().await.is_ok());
}
