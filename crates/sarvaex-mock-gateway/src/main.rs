use sarvaex_mock_gateway::{API_KEY_READONLY, API_KEY_TRADER, MockGateway, PASSWORD, USER_TRADER};

#[tokio::main]
async fn main() {
    let bind = std::env::args()
        .nth(2)
        .filter(|_| std::env::args().nth(1).as_deref() == Some("--bind"))
        .unwrap_or_else(|| "127.0.0.1:18080".to_owned());
    let listener = tokio::net::TcpListener::bind(&bind).await.expect("bind");
    eprintln!("sarvaex-mock-gateway listening on http://{bind}");
    eprintln!("  trading API key : {API_KEY_TRADER}");
    eprintln!("  read-only key   : {API_KEY_READONLY}");
    eprintln!("  login           : user_id {USER_TRADER} / password {PASSWORD}");
    eprintln!(
        "  try: SARVAEX_BASE_URL=http://{bind} SARVAEX_API_KEY={API_KEY_TRADER} sarvaex markets list"
    );
    axum::serve(listener, MockGateway::new().router().into_make_service())
        .await
        .expect("serve");
}
