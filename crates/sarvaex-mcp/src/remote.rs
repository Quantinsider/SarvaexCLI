//! Remote (Streamable HTTP) MCP server with OAuth 2.1, so Claude.ai, ChatGPT and
//! other hosted assistants can add SarvaEX as a custom connector.
//!
//! Flow for a web assistant:
//! 1. It POSTs to `/mcp` without a token and receives `401` with
//!    `WWW-Authenticate: Bearer resource_metadata=…`.
//! 2. It reads `/.well-known/oauth-protected-resource`, then
//!    `/.well-known/oauth-authorization-server`, and registers itself at
//!    `/oauth/register` (dynamic client registration).
//! 3. The user is sent to `/oauth/authorize`, signs in with their SarvaEX
//!    account (or pastes an existing API key) and chooses whether to allow
//!    trading. The server mints a scoped SarvaEX API key for this connection,
//!    stores it encrypted, and redirects back with an authorization code.
//! 4. The assistant exchanges the code (PKCE S256) at `/oauth/token` and uses the
//!    access token on every `/mcp` request. Each tool call runs with that
//!    user's SarvaEX API key.
//!
//! Programmatic clients that can set headers may skip OAuth entirely and send
//! `Authorization: Bearer svx_live_…` directly.

use crate::store::{Store, pkce_matches, sha256_b64};
use crate::tools::{McpState, Principal, SarvaexMcp};
use anyhow::{Context, Result};
use axum::extract::{Form, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use sarvaex_sdk::{Credential, READ_SCOPES};
use serde::Deserialize;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use url::Url;

pub const SCOPE_READ: &str = "read";
pub const SCOPE_TRADE: &str = "trade";

#[derive(Debug, Clone)]
pub struct RemoteOptions {
    pub bind: SocketAddr,
    /// Public origin clients use, e.g. `https://mcp.sarvaex.com`.
    pub public_url: Url,
    pub state_dir: PathBuf,
    /// Extra `Host` header values to accept (the public URL host is always included).
    pub allowed_hosts: Vec<String>,
    /// Disable Host validation entirely (only behind a trusted reverse proxy).
    pub allow_any_host: bool,
    /// Let users sign in with email/password on the consent page.
    pub allow_password_login: bool,
    /// Let users paste an existing API key on the consent page.
    pub allow_api_key_paste: bool,
}

#[derive(Clone)]
pub struct HttpState {
    pub mcp: Arc<McpState>,
    pub store: Arc<Store>,
    pub issuer: String,
    pub opts: Arc<RemoteOptions>,
}

impl HttpState {
    fn url(&self, path: &str) -> String {
        format!("{}{}", self.issuer, path)
    }
    fn resource(&self) -> String {
        self.url("/mcp")
    }
}

/// Build the complete axum application.
pub fn build_app(mcp: Arc<McpState>, store: Arc<Store>, opts: RemoteOptions) -> Router {
    let issuer = opts.public_url.as_str().trim_end_matches('/').to_owned();
    let state = HttpState {
        mcp: mcp.clone(),
        store,
        issuer,
        opts: Arc::new(opts),
    };

    let mut hosts: Vec<String> = vec!["localhost".into(), "127.0.0.1".into(), "::1".into()];
    if let Some(h) = state.opts.public_url.host_str() {
        hosts.push(h.to_owned());
        if let Some(p) = state.opts.public_url.port() {
            hosts.push(format!("{h}:{p}"));
        }
    }
    hosts.extend(state.opts.allowed_hosts.iter().cloned());
    // Stateless + JSON responses: every POST is self-contained, so the server
    // survives restarts and can run as several replicas behind a load balancer.
    let mut config = StreamableHttpServerConfig::default()
        .with_allowed_hosts(hosts)
        .with_legacy_session_mode(false)
        .with_json_response(true);
    if state.opts.allow_any_host {
        config = config.disable_allowed_hosts();
    }
    let mcp_for_factory = mcp.clone();
    let service: StreamableHttpService<SarvaexMcp, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(SarvaexMcp::new(mcp_for_factory.clone())),
            Arc::new(LocalSessionManager::default()),
            config,
        );

    let mcp_router = Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn_with_state(state.clone(), mcp_auth));

    let public = Router::new()
        .route("/", get(landing))
        .route(
            "/healthz",
            get(|| async { Json(json!({"status": "ok", "service": "sarvaex-mcp"})) }),
        )
        .route(
            "/.well-known/oauth-protected-resource",
            get(protected_resource_metadata),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(protected_resource_metadata),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(authorization_server_metadata),
        )
        .route(
            "/.well-known/oauth-authorization-server/mcp",
            get(authorization_server_metadata),
        )
        .route(
            "/.well-known/openid-configuration",
            get(authorization_server_metadata),
        )
        .route("/oauth/register", post(register_client))
        .route("/oauth/authorize", get(authorize_get).post(authorize_post))
        .route("/oauth/token", post(token))
        .route("/oauth/revoke", post(revoke))
        .layer(CorsLayer::permissive())
        .with_state(state.clone());

    public.merge(mcp_router)
}

/// Run the remote server until the process is signalled to stop.
pub async fn serve(mcp: Arc<McpState>, opts: RemoteOptions) -> Result<()> {
    let secret = crate::store::load_or_create_secret(&opts.state_dir)?;
    let store = Arc::new(Store::open(&opts.state_dir, secret)?);
    let _ = store.purge_expired();
    let bind = opts.bind;
    let public = opts.public_url.clone();
    let app = build_app(mcp, store, opts);
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    tracing::info!(%bind, public = %public, "sarvaex-mcp remote server listening");
    eprintln!("sarvaex-mcp listening on http://{bind}  (public: {public})");
    eprintln!(
        "MCP endpoint: {}mcp   OAuth metadata: {}.well-known/oauth-authorization-server",
        public, public
    );
    axum::serve(listener, app.into_make_service())
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Auth middleware for /mcp
// ---------------------------------------------------------------------------

async fn mcp_auth(State(st): State<HttpState>, mut req: Request, next: Next) -> Response {
    let token = bearer_token(req.headers());
    match resolve_principal(&st, token.as_deref()).await {
        Ok(p) => {
            req.extensions_mut().insert(p);
            next.run(req).await
        }
        Err(reason) => unauthorized(&st, &reason),
    }
}

fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, rest) = v.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let t = rest.trim();
    (!t.is_empty()).then(|| t.to_owned())
}

async fn resolve_principal(
    st: &HttpState,
    token: Option<&str>,
) -> std::result::Result<Principal, String> {
    let token = token.ok_or_else(|| "missing bearer token".to_owned())?;
    if token.starts_with("svx_") {
        // Direct SarvaEX API key. Scope enforcement happens at the gateway.
        return Ok(Principal {
            credential: Credential::ApiKey(token.to_owned()),
            user_id: None,
            can_trade: true,
            grant_id: None,
        });
    }
    let rec = st
        .store
        .lookup_token(token, "access")
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "invalid or expired access token".to_owned())?;
    let grant = st
        .store
        .get_grant(&rec.grant_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "grant revoked".to_owned())?;
    let scope_allows_trade = rec.scope.split_whitespace().any(|s| s == SCOPE_TRADE);
    Ok(Principal {
        credential: Credential::ApiKey(grant.api_key),
        user_id: Some(grant.user_id),
        can_trade: grant.can_trade && scope_allows_trade,
        grant_id: Some(grant.grant_id),
    })
}

fn unauthorized(st: &HttpState, reason: &str) -> Response {
    let www = format!(
        "Bearer realm=\"sarvaex-mcp\", error=\"invalid_token\", error_description=\"{}\", resource_metadata=\"{}\"",
        reason.replace('"', "'"),
        st.url("/.well-known/oauth-protected-resource")
    );
    let mut resp = (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": "invalid_token", "error_description": reason })),
    )
        .into_response();
    if let Ok(v) = HeaderValue::from_str(&www) {
        resp.headers_mut().insert(header::WWW_AUTHENTICATE, v);
    }
    resp
}

// ---------------------------------------------------------------------------
// Metadata
// ---------------------------------------------------------------------------

async fn protected_resource_metadata(State(st): State<HttpState>) -> Json<Value> {
    Json(json!({
        "resource": st.resource(),
        "authorization_servers": [st.issuer],
        "scopes_supported": [SCOPE_READ, SCOPE_TRADE],
        "bearer_methods_supported": ["header"],
        "resource_name": "SarvaEX MCP",
        "resource_documentation": st.url("/"),
    }))
}

async fn authorization_server_metadata(State(st): State<HttpState>) -> Json<Value> {
    Json(json!({
        "issuer": st.issuer,
        "authorization_endpoint": st.url("/oauth/authorize"),
        "token_endpoint": st.url("/oauth/token"),
        "registration_endpoint": st.url("/oauth/register"),
        "revocation_endpoint": st.url("/oauth/revoke"),
        "scopes_supported": [SCOPE_READ, SCOPE_TRADE],
        "response_types_supported": ["code"],
        "response_modes_supported": ["query"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "token_endpoint_auth_methods_supported": ["none", "client_secret_post", "client_secret_basic"],
        "revocation_endpoint_auth_methods_supported": ["none", "client_secret_post", "client_secret_basic"],
        "code_challenge_methods_supported": ["S256"],
        "service_documentation": st.url("/"),
    }))
}

// ---------------------------------------------------------------------------
// Dynamic client registration (RFC 7591)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RegisterRequest {
    #[serde(default)]
    client_name: Option<String>,
    #[serde(default)]
    redirect_uris: Vec<String>,
    #[serde(default)]
    token_endpoint_auth_method: Option<String>,
    #[serde(default)]
    grant_types: Option<Vec<String>>,
    #[serde(default)]
    response_types: Option<Vec<String>>,
    #[serde(default)]
    scope: Option<String>,
}

async fn register_client(
    State(st): State<HttpState>,
    body: Result<Json<RegisterRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(Json(req)) = body else {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_client_metadata",
            "body must be JSON",
        );
    };
    if req.redirect_uris.is_empty() {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_redirect_uri",
            "at least one redirect_uri is required",
        );
    }
    for uri in &req.redirect_uris {
        match Url::parse(uri) {
            Ok(u)
                if u.scheme() == "https"
                    || u.host_str().is_some_and(is_loopback_host)
                    || u.scheme() != "http" => {}
            _ => {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_redirect_uri",
                    &format!("redirect_uri must be https (or http on loopback): {uri}"),
                );
            }
        }
    }
    let method = req
        .token_endpoint_auth_method
        .clone()
        .unwrap_or_else(|| "client_secret_basic".into());
    let issue_secret = method != "none";
    let name = req
        .client_name
        .clone()
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| "MCP client".into());
    let (client, secret) =
        match st
            .store
            .register_client(name.trim(), &req.redirect_uris, issue_secret)
        {
            Ok(v) => v,
            Err(e) => {
                return oauth_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "server_error",
                    &e.to_string(),
                );
            }
        };
    let mut out = json!({
        "client_id": client.client_id,
        "client_id_issued_at": crate::store::now(),
        "client_name": client.client_name,
        "redirect_uris": client.redirect_uris,
        "token_endpoint_auth_method": method,
        "grant_types": req.grant_types.unwrap_or_else(|| vec!["authorization_code".into(), "refresh_token".into()]),
        "response_types": req.response_types.unwrap_or_else(|| vec!["code".into()]),
        "scope": req.scope.unwrap_or_else(|| format!("{SCOPE_READ} {SCOPE_TRADE}")),
    });
    if let Some(s) = secret {
        out["client_secret"] = json!(s);
        out["client_secret_expires_at"] = json!(0);
    }
    (StatusCode::CREATED, Json(out)).into_response()
}

fn is_loopback_host(h: &str) -> bool {
    matches!(h, "localhost" | "127.0.0.1" | "[::1]" | "::1")
}

fn oauth_error(status: StatusCode, error: &str, description: &str) -> Response {
    (
        status,
        Json(json!({ "error": error, "error_description": description })),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Authorization endpoint
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Default)]
struct AuthorizeParams {
    #[serde(default)]
    response_type: String,
    #[serde(default)]
    client_id: String,
    #[serde(default)]
    redirect_uri: String,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    code_challenge: Option<String>,
    #[serde(default)]
    code_challenge_method: Option<String>,
    #[serde(default)]
    resource: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct AuthorizeForm {
    #[serde(flatten)]
    oauth: AuthorizeParams,
    #[serde(default)]
    mode: String,
    #[serde(default)]
    identifier: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    allow_trade: Option<String>,
    #[serde(default)]
    label: String,
}

/// Validate client + redirect URI; on failure the response must NOT redirect.
fn validate_client(
    st: &HttpState,
    p: &AuthorizeParams,
) -> std::result::Result<crate::store::OAuthClient, String> {
    let client = st
        .store
        .get_client(&p.client_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "unknown client_id; the assistant must register first".to_owned())?;
    if !client.redirect_uris.iter().any(|u| u == &p.redirect_uri) {
        return Err("redirect_uri is not registered for this client".into());
    }
    Ok(client)
}

fn redirect_error(p: &AuthorizeParams, error: &str, description: &str) -> Response {
    let mut url = match Url::parse(&p.redirect_uri) {
        Ok(u) => u,
        Err(_) => return error_page(StatusCode::BAD_REQUEST, "Invalid redirect_uri"),
    };
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("error", error)
            .append_pair("error_description", description);
        if let Some(s) = &p.state {
            q.append_pair("state", s);
        }
    }
    Redirect::to(url.as_str()).into_response()
}

async fn authorize_get(State(st): State<HttpState>, Query(p): Query<AuthorizeParams>) -> Response {
    let client = match validate_client(&st, &p) {
        Ok(c) => c,
        Err(msg) => return error_page(StatusCode::BAD_REQUEST, &msg),
    };
    if p.response_type != "code" {
        return redirect_error(
            &p,
            "unsupported_response_type",
            "only response_type=code is supported",
        );
    }
    if p.code_challenge.as_deref().unwrap_or("").is_empty()
        || p.code_challenge_method.as_deref().unwrap_or("S256") != "S256"
    {
        return redirect_error(
            &p,
            "invalid_request",
            "PKCE with code_challenge_method=S256 is required",
        );
    }
    Html(consent_page(&st, &p, &client.client_name, None)).into_response()
}

async fn authorize_post(State(st): State<HttpState>, Form(form): Form<AuthorizeForm>) -> Response {
    let p = form.oauth.clone();
    let client = match validate_client(&st, &p) {
        Ok(c) => c,
        Err(msg) => return error_page(StatusCode::BAD_REQUEST, &msg),
    };
    if p.code_challenge.as_deref().unwrap_or("").is_empty() {
        return redirect_error(&p, "invalid_request", "PKCE code_challenge is required");
    }
    let allow_trade = form
        .allow_trade
        .as_deref()
        .is_some_and(|v| v == "on" || v == "true" || v == "1");
    let label = if form.label.trim().is_empty() {
        format!("mcp:{}", client.client_name)
    } else {
        form.label.trim().chars().take(64).collect()
    };
    let requested_scope = p.scope.clone().unwrap_or_default();
    let granted_scope = if allow_trade {
        format!("{SCOPE_READ} {SCOPE_TRADE}")
    } else {
        SCOPE_READ.to_owned()
    };
    let _ = requested_scope;

    let outcome: std::result::Result<(String, String, Option<String>, bool), String> =
        match form.mode.as_str() {
            "password" if st.opts.allow_password_login => {
                login_and_mint_key(
                    &st,
                    form.identifier.trim(),
                    &form.password,
                    allow_trade,
                    &label,
                )
                .await
            }
            "apikey" if st.opts.allow_api_key_paste => {
                verify_pasted_key(&st, form.api_key.trim(), allow_trade).await
            }
            _ => Err("unsupported sign-in mode".into()),
        };
    let (user_id, api_key, key_id, can_trade) = match outcome {
        Ok(v) => v,
        Err(msg) => {
            return (
                StatusCode::UNAUTHORIZED,
                Html(consent_page(&st, &p, &client.client_name, Some(&msg))),
            )
                .into_response();
        }
    };
    let grant_id =
        match st
            .store
            .create_grant(&user_id, &api_key, key_id.as_deref(), can_trade, &label)
        {
            Ok(g) => g,
            Err(e) => return error_page(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
        };
    let code = match st.store.create_code(
        &client.client_id,
        &p.redirect_uri,
        p.code_challenge.as_deref().unwrap_or(""),
        &granted_scope,
        &grant_id,
    ) {
        Ok(c) => c,
        Err(e) => return error_page(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    let mut url = match Url::parse(&p.redirect_uri) {
        Ok(u) => u,
        Err(_) => return error_page(StatusCode::BAD_REQUEST, "Invalid redirect_uri"),
    };
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("code", &code);
        if let Some(s) = &p.state {
            q.append_pair("state", s);
        }
    }
    tracing::info!(user = %user_id, client = %client.client_name, can_trade, "authorized MCP connection");
    Redirect::to(url.as_str()).into_response()
}

/// Sign in with SarvaEX credentials and mint a dedicated API key for this connection.
async fn login_and_mint_key(
    st: &HttpState,
    identifier: &str,
    password: &str,
    allow_trade: bool,
    label: &str,
) -> std::result::Result<(String, String, Option<String>, bool), String> {
    if identifier.is_empty() || password.is_empty() {
        return Err("enter your email (or user id) and password".into());
    }
    let anon = st.mcp.base.with_swapped_credential(Credential::None);
    let login = anon
        .login(identifier, password)
        .await
        .map_err(|e| match e.status() {
            Some(401) | Some(400) => "invalid email/user id or password".to_owned(),
            _ => format!("SarvaEX login failed: {e}"),
        })?;
    let session = anon.with_swapped_credential(Credential::Bearer(login.token.clone()));
    let mut scopes: Vec<&str> = READ_SCOPES.to_vec();
    scopes.push("websocket:read");
    if allow_trade {
        scopes.push("trading:write");
    }
    let created = session
        .create_api_key(label, &scopes, None)
        .await
        .map_err(|e| format!("could not create an API key: {e}"))?;
    let user_id = if login.user_id.is_empty() {
        session
            .profile()
            .await
            .map(|p| p.user_id)
            .unwrap_or_default()
    } else {
        login.user_id
    };
    Ok((user_id, created.key, Some(created.key_id), allow_trade))
}

/// Validate a pasted API key and detect whether it can trade.
async fn verify_pasted_key(
    st: &HttpState,
    api_key: &str,
    allow_trade: bool,
) -> std::result::Result<(String, String, Option<String>, bool), String> {
    if !api_key.starts_with("svx_") {
        return Err("that does not look like a SarvaEX API key (expected svx_live_…)".into());
    }
    let client = st
        .mcp
        .base
        .with_swapped_credential(Credential::ApiKey(api_key.to_owned()));
    let profile = client.profile().await.map_err(|e| match e.status() {
        Some(401) => "API key rejected by SarvaEX".to_owned(),
        _ => format!("could not verify key: {e}"),
    })?;
    let prefix: String = api_key.chars().take(17).collect();
    let key_can_trade = match client.list_api_keys().await {
        Ok(keys) => keys
            .iter()
            .find(|k| k.key_prefix == prefix)
            .map(|k| k.scopes.iter().any(|s| s == "trading:write" || s == "*"))
            .unwrap_or(true),
        Err(_) => true,
    };
    Ok((
        profile.user_id,
        api_key.to_owned(),
        None,
        allow_trade && key_can_trade,
    ))
}

// ---------------------------------------------------------------------------
// Token endpoint
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
struct TokenForm {
    #[serde(default)]
    grant_type: String,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    redirect_uri: Option<String>,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    client_secret: Option<String>,
    #[serde(default)]
    code_verifier: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
}

fn client_credentials(headers: &HeaderMap, form: &TokenForm) -> (Option<String>, Option<String>) {
    if let Some(v) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        && let Some(b64) = v.strip_prefix("Basic ")
        && let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64.trim())
        && let Ok(s) = String::from_utf8(bytes)
        && let Some((id, secret)) = s.split_once(':')
    {
        let id = percent_decode(id);
        let secret = percent_decode(secret);
        return (Some(id), Some(secret));
    }
    (form.client_id.clone(), form.client_secret.clone())
}

fn percent_decode(s: &str) -> String {
    url::form_urlencoded::parse(s.as_bytes())
        .map(|(k, v)| format!("{k}{v}"))
        .collect::<String>()
}

async fn token(
    State(st): State<HttpState>,
    headers: HeaderMap,
    Form(form): Form<TokenForm>,
) -> Response {
    let (client_id, client_secret) = client_credentials(&headers, &form);
    let Some(client_id) = client_id.filter(|c| !c.is_empty()) else {
        return oauth_error(
            StatusCode::UNAUTHORIZED,
            "invalid_client",
            "client_id is required",
        );
    };
    let client = match st.store.get_client(&client_id) {
        Ok(Some(c)) => c,
        Ok(None) => {
            return oauth_error(
                StatusCode::UNAUTHORIZED,
                "invalid_client",
                "unknown client_id",
            );
        }
        Err(e) => {
            return oauth_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                &e.to_string(),
            );
        }
    };
    if let Some(hash) = &client.client_secret_hash {
        match client_secret.as_deref() {
            Some(s) if &sha256_b64(s) == hash => {}
            Some(_) => {
                return oauth_error(
                    StatusCode::UNAUTHORIZED,
                    "invalid_client",
                    "client secret mismatch",
                );
            }
            None => {
                return oauth_error(
                    StatusCode::UNAUTHORIZED,
                    "invalid_client",
                    "client secret required",
                );
            }
        }
    }

    match form.grant_type.as_str() {
        "authorization_code" => {
            let Some(code) = form.code.as_deref().filter(|c| !c.is_empty()) else {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "code is required",
                );
            };
            let Some(verifier) = form.code_verifier.as_deref().filter(|c| !c.is_empty()) else {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "code_verifier is required (PKCE)",
                );
            };
            let record = match st.store.consume_code(code) {
                Ok(Some(r)) => r,
                Ok(None) => {
                    return oauth_error(
                        StatusCode::BAD_REQUEST,
                        "invalid_grant",
                        "authorization code is invalid, expired or already used",
                    );
                }
                Err(e) => {
                    return oauth_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "server_error",
                        &e.to_string(),
                    );
                }
            };
            if record.client_id != client.client_id {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_grant",
                    "code was issued to a different client",
                );
            }
            if let Some(ru) = form.redirect_uri.as_deref()
                && !ru.is_empty()
                && ru != record.redirect_uri
            {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_grant",
                    "redirect_uri mismatch",
                );
            }
            if !pkce_matches(verifier, &record.code_challenge) {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_grant",
                    "PKCE verification failed",
                );
            }
            match st
                .store
                .issue_tokens(&record.grant_id, &client.client_id, &record.scope)
            {
                Ok(t) => token_response(t),
                Err(e) => oauth_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "server_error",
                    &e.to_string(),
                ),
            }
        }
        "refresh_token" => {
            let Some(rt) = form.refresh_token.as_deref().filter(|c| !c.is_empty()) else {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "refresh_token is required",
                );
            };
            match st.store.refresh(rt, &client.client_id) {
                Ok(Some(t)) => token_response(t),
                Ok(None) => oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_grant",
                    "refresh token is invalid, expired or revoked",
                ),
                Err(e) => oauth_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "server_error",
                    &e.to_string(),
                ),
            }
        }
        other => oauth_error(
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
            &format!("grant_type '{other}' is not supported"),
        ),
    }
}

fn token_response(t: crate::store::IssuedTokens) -> Response {
    let mut resp = Json(json!({
        "access_token": t.access_token,
        "token_type": "Bearer",
        "expires_in": t.expires_in,
        "refresh_token": t.refresh_token,
        "scope": t.scope,
    }))
    .into_response();
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp.headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    resp
}

#[derive(Debug, Deserialize, Default)]
struct RevokeForm {
    #[serde(default)]
    token: String,
}

/// RFC 7009. Revoking any token of a grant revokes the whole connection, including
/// the SarvaEX API key this server created for it.
async fn revoke(State(st): State<HttpState>, Form(form): Form<RevokeForm>) -> Response {
    if form.token.is_empty() {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "token is required",
        );
    }
    if let Ok(Some(grant_id)) = st.store.revoke_token(&form.token) {
        if let Ok(Some(grant)) = st.store.get_grant(&grant_id)
            && let Some(key_id) = &grant.sarvaex_key_id
        {
            let c = st
                .mcp
                .base
                .with_swapped_credential(Credential::ApiKey(grant.api_key.clone()));
            if let Err(e) = c.revoke_api_key(key_id).await {
                tracing::warn!(error = %e, "could not revoke SarvaEX API key for grant");
            }
        }
        let _ = st.store.revoke_grant(&grant_id);
    }
    StatusCode::OK.into_response()
}

// ---------------------------------------------------------------------------
// HTML
// ---------------------------------------------------------------------------

const CSS: &str = r#"
:root{color-scheme:light dark;--bg:#f6f7f9;--card:#fff;--fg:#111;--muted:#5b6472;--accent:#0f766e;--border:#dfe3e8;--err:#b42318}
@media (prefers-color-scheme:dark){:root{--bg:#0e1116;--card:#171b22;--fg:#e6e8eb;--muted:#9aa3ad;--accent:#2dd4bf;--border:#2a313b;--err:#f97066}}
*{box-sizing:border-box}body{margin:0;font:15px/1.5 system-ui,-apple-system,Segoe UI,Roboto,sans-serif;background:var(--bg);color:var(--fg);padding:24px}
.card{max-width:520px;margin:32px auto;background:var(--card);border:1px solid var(--border);border-radius:12px;padding:28px}
h1{font-size:20px;margin:0 0 4px}p{margin:8px 0;color:var(--muted)}label{display:block;font-weight:600;margin:14px 0 4px}
input[type=text],input[type=password],input[type=email]{width:100%;padding:10px;border:1px solid var(--border);border-radius:8px;background:transparent;color:var(--fg);font:inherit}
button{margin-top:16px;width:100%;padding:11px;border:0;border-radius:8px;background:var(--accent);color:#fff;font:inherit;font-weight:600;cursor:pointer}
.check{display:flex;gap:10px;align-items:flex-start;margin-top:14px}.check input{margin-top:4px}.check span{font-weight:400}
.err{background:rgba(180,35,24,.08);border:1px solid var(--err);color:var(--err);padding:10px;border-radius:8px;margin-top:12px}
hr{border:0;border-top:1px solid var(--border);margin:22px 0}code{font-family:ui-monospace,Menlo,monospace;font-size:13px;background:rgba(127,127,127,.12);padding:1px 5px;border-radius:4px}
small{color:var(--muted)}
"#;

fn esc(s: &str) -> String {
    html_escape::encode_double_quoted_attribute(s).into_owned()
}
fn esc_t(s: &str) -> String {
    html_escape::encode_text(s).into_owned()
}

fn hidden_oauth_fields(p: &AuthorizeParams) -> String {
    let mut out = String::new();
    let mut push = |k: &str, v: &str| {
        out.push_str(&format!(
            "<input type=\"hidden\" name=\"{k}\" value=\"{}\">",
            esc(v)
        ))
    };
    push("response_type", &p.response_type);
    push("client_id", &p.client_id);
    push("redirect_uri", &p.redirect_uri);
    if let Some(v) = &p.scope {
        push("scope", v);
    }
    if let Some(v) = &p.state {
        push("state", v);
    }
    if let Some(v) = &p.code_challenge {
        push("code_challenge", v);
    }
    if let Some(v) = &p.code_challenge_method {
        push("code_challenge_method", v);
    }
    if let Some(v) = &p.resource {
        push("resource", v);
    }
    out
}

fn consent_page(
    st: &HttpState,
    p: &AuthorizeParams,
    client_name: &str,
    error: Option<&str>,
) -> String {
    let hidden = hidden_oauth_fields(p);
    let err_html = error
        .map(|e| format!("<div class=\"err\">{}</div>", esc_t(e)))
        .unwrap_or_default();
    let gateway = st.mcp.base.base_url().as_str().trim_end_matches('/');
    let trade_box = |id: &str| {
        format!(
            "<label class=\"check\"><input type=\"checkbox\" name=\"allow_trade\" id=\"{id}\"><span><strong>Allow trading</strong> — place and cancel orders. Leave unchecked for read-only access to markets, balance and positions.</span></label>"
        )
    };
    let password_form = if st.opts.allow_password_login {
        format!(
            r#"<form method="post" action="/oauth/authorize">{hidden}<input type="hidden" name="mode" value="password">
<label for="identifier">Email or user id</label><input id="identifier" name="identifier" type="text" autocomplete="username" required>
<label for="password">Password</label><input id="password" name="password" type="password" autocomplete="current-password" required>
<label for="label">Connection name <small>(shown in your API keys)</small></label><input id="label" name="label" type="text" value="{label}">
{trade}<button type="submit">Sign in and connect</button>
<p><small>A dedicated API key is created for this connection. Revoke it any time from your SarvaEX profile.</small></p></form>"#,
            label = esc(&format!("mcp:{client_name}")),
            trade = trade_box("t1")
        )
    } else {
        String::new()
    };
    let key_form = if st.opts.allow_api_key_paste {
        format!(
            r#"{sep}<form method="post" action="/oauth/authorize">{hidden}<input type="hidden" name="mode" value="apikey">
<label for="api_key">Use an existing API key</label><input id="api_key" name="api_key" type="password" placeholder="svx_live_…" autocomplete="off">
{trade}<button type="submit">Connect with API key</button>
<p><small>Create keys at <code>{gw}</code> → Profile → API keys. Trading requires the <code>trading:write</code> scope.</small></p></form>"#,
            sep = if st.opts.allow_password_login {
                "<hr>"
            } else {
                ""
            },
            trade = trade_box("t2"),
            gw = esc_t(gateway)
        )
    } else {
        String::new()
    };
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Connect {name} to SarvaEX</title><style>{css}</style></head>
<body><div class="card"><h1>Connect <strong>{name}</strong> to SarvaEX</h1>
<p>{name} is asking to access your SarvaEX account through MCP. Exchange: <code>{gw}</code></p>{err}
{pw}{key}
</div></body></html>"#,
        name = esc_t(client_name),
        css = CSS,
        gw = esc_t(gateway),
        err = err_html,
        pw = password_form,
        key = key_form
    )
}

fn error_page(status: StatusCode, msg: &str) -> Response {
    let body = format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>SarvaEX MCP — error</title><style>{CSS}</style></head><body><div class="card"><h1>Could not continue</h1><div class="err">{}</div><p>Close this window and try connecting again from your assistant.</p></div></body></html>"#,
        esc_t(msg)
    );
    (status, Html(body)).into_response()
}

async fn landing(State(st): State<HttpState>) -> Html<String> {
    let mcp_url = st.resource();
    let gw = st
        .mcp
        .base
        .base_url()
        .as_str()
        .trim_end_matches('/')
        .to_owned();
    Html(format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>SarvaEX MCP server</title><style>{CSS}</style></head>
<body><div class="card"><h1>SarvaEX MCP server</h1>
<p>Lets AI assistants read markets and trade on <code>{gw}</code> on your behalf, with your explicit consent per connection.</p>
<label>MCP endpoint</label><code>{mcp}</code>
<hr>
<label>Claude (web / desktop)</label><p>Settings → Connectors → <em>Add custom connector</em> → paste the endpoint above. You will be asked to sign in to SarvaEX and choose whether to allow trading.</p>
<label>ChatGPT</label><p>Settings → Connectors → <em>Create</em> (developer mode) → paste the endpoint, authentication <em>OAuth</em>. Sign in when prompted.</p>
<label>Claude Code</label><p><code>claude mcp add --transport http sarvaex {mcp}</code></p>
<label>Any MCP client with headers</label><p>Send <code>Authorization: Bearer svx_live_…</code> with your SarvaEX API key directly.</p>
<hr>
<p><small>OAuth metadata: <code>/.well-known/oauth-authorization-server</code> · Health: <code>/healthz</code></small></p>
</div></body></html>"#,
        gw = esc_t(&gw),
        mcp = esc_t(&mcp_url)
    ))
}
