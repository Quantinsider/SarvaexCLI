//! Persistent state for the remote MCP server: OAuth clients, authorization
//! codes, grants (encrypted SarvaEX API keys) and access/refresh tokens.
//!
//! Tokens and codes are stored as SHA-256 hashes. The SarvaEX API key behind a
//! grant is encrypted with XChaCha20-Poly1305 under a server secret, because
//! the server must present the plaintext key to the gateway on each call.

use anyhow::{Context, Result, anyhow};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use uuid::Uuid;

pub const ACCESS_TOKEN_TTL_SECS: i64 = 60 * 60;
pub const REFRESH_TOKEN_TTL_SECS: i64 = 60 * 60 * 24 * 30;
pub const AUTH_CODE_TTL_SECS: i64 = 10 * 60;

#[derive(Debug, Clone)]
pub struct OAuthClient {
    pub client_id: String,
    pub client_secret_hash: Option<String>,
    pub client_name: String,
    pub redirect_uris: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct AuthCode {
    pub client_id: String,
    pub redirect_uri: String,
    pub code_challenge: String,
    pub scope: String,
    pub grant_id: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone)]
pub struct Grant {
    pub grant_id: String,
    pub user_id: String,
    pub api_key: String,
    /// `key_id` of the SarvaEX API key if this server created it (so it can be revoked).
    pub sarvaex_key_id: Option<String>,
    pub can_trade: bool,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct TokenRecord {
    pub kind: String,
    pub grant_id: String,
    pub client_id: String,
    pub scope: String,
    pub expires_at: i64,
}

/// Issued token pair.
#[derive(Debug, Clone)]
pub struct IssuedTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: i64,
    pub scope: String,
}

pub struct Store {
    conn: Mutex<Connection>,
    cipher: XChaCha20Poly1305,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Store")
    }
}

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

pub fn random_token(prefix: &str) -> String {
    format!(
        "{prefix}{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    )
}

pub fn sha256_b64(input: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(input.as_bytes()))
}

/// PKCE S256: `BASE64URL(SHA256(verifier)) == challenge`.
pub fn pkce_matches(verifier: &str, challenge: &str) -> bool {
    sha256_b64(verifier) == challenge.trim_end_matches('=')
}

/// Load the 32-byte server secret from `SARVAEX_MCP_SECRET` (hex or base64),
/// or create and persist one under `state_dir/secret.key`.
pub fn load_or_create_secret(state_dir: &Path) -> Result<[u8; 32]> {
    if let Ok(v) = std::env::var("SARVAEX_MCP_SECRET") {
        return parse_secret(v.trim())
            .context("SARVAEX_MCP_SECRET must be 32 bytes as hex or base64");
    }
    let path = state_dir.join("secret.key");
    if path.exists() {
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        return parse_secret(text.trim())
            .with_context(|| format!("{} is not a valid 32-byte secret", path.display()));
    }
    let mut key = [0u8; 32];
    key[..16].copy_from_slice(Uuid::new_v4().as_bytes());
    key[16..].copy_from_slice(Uuid::new_v4().as_bytes());
    std::fs::create_dir_all(state_dir)?;
    std::fs::write(&path, STANDARD.encode(key))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    tracing::warn!(path = %path.display(), "generated a new server secret; back it up or set SARVAEX_MCP_SECRET");
    Ok(key)
}

fn parse_secret(s: &str) -> Result<[u8; 32]> {
    let bytes: Vec<u8> = if s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        (0..32)
            .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16))
            .collect::<std::result::Result<_, _>>()?
    } else {
        STANDARD
            .decode(s)
            .or_else(|_| URL_SAFE_NO_PAD.decode(s))
            .map_err(|e| anyhow!("secret is not hex or base64: {e}"))?
    };
    bytes
        .try_into()
        .map_err(|_| anyhow!("secret must decode to exactly 32 bytes"))
}

impl Store {
    pub fn open(state_dir: &Path, secret: [u8; 32]) -> Result<Self> {
        std::fs::create_dir_all(state_dir)?;
        let path: PathBuf = state_dir.join("sarvaex-mcp.sqlite");
        let conn =
            Connection::open(&path).with_context(|| format!("opening {}", path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS oauth_clients (
                client_id TEXT PRIMARY KEY,
                client_secret_hash TEXT,
                client_name TEXT NOT NULL,
                redirect_uris TEXT NOT NULL,
                created_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS grants (
                grant_id TEXT PRIMARY KEY,
                user_id TEXT NOT NULL,
                api_key_enc BLOB NOT NULL,
                sarvaex_key_id TEXT,
                can_trade INTEGER NOT NULL,
                label TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                revoked_at INTEGER
             );
             CREATE TABLE IF NOT EXISTS oauth_codes (
                code_hash TEXT PRIMARY KEY,
                client_id TEXT NOT NULL,
                redirect_uri TEXT NOT NULL,
                code_challenge TEXT NOT NULL,
                scope TEXT NOT NULL,
                grant_id TEXT NOT NULL,
                expires_at INTEGER NOT NULL,
                used INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS oauth_tokens (
                token_hash TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                grant_id TEXT NOT NULL,
                client_id TEXT NOT NULL,
                scope TEXT NOT NULL,
                expires_at INTEGER NOT NULL,
                revoked_at INTEGER
             );
             CREATE INDEX IF NOT EXISTS idx_tokens_grant ON oauth_tokens(grant_id);",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
            cipher: XChaCha20Poly1305::new((&secret).into()),
        })
    }

    /// In-memory store for tests.
    pub fn in_memory(secret: [u8; 32]) -> Result<Self> {
        let dir =
            std::env::temp_dir().join(format!("sarvaex-mcp-test-{}", Uuid::new_v4().simple()));
        Self::open(&dir, secret)
    }

    fn encrypt(&self, plaintext: &str) -> Result<Vec<u8>> {
        let mut nonce_bytes = [0u8; 24];
        nonce_bytes[..16].copy_from_slice(Uuid::new_v4().as_bytes());
        nonce_bytes[16..].copy_from_slice(&Uuid::new_v4().as_bytes()[..8]);
        let nonce = XNonce::from(nonce_bytes);
        let ct = self
            .cipher
            .encrypt(&nonce, plaintext.as_bytes())
            .map_err(|_| anyhow!("encryption failed"))?;
        let mut out = nonce_bytes.to_vec();
        out.extend_from_slice(&ct);
        Ok(out)
    }

    fn decrypt(&self, blob: &[u8]) -> Result<String> {
        if blob.len() < 25 {
            return Err(anyhow!("ciphertext too short"));
        }
        let (nonce, ct) = blob.split_at(24);
        let nonce = XNonce::try_from(nonce).map_err(|_| anyhow!("bad nonce"))?;
        let pt = self
            .cipher
            .decrypt(&nonce, ct)
            .map_err(|_| anyhow!("decryption failed (wrong server secret?)"))?;
        Ok(String::from_utf8(pt)?)
    }

    // ---- clients ---------------------------------------------------------

    pub fn register_client(
        &self,
        client_name: &str,
        redirect_uris: &[String],
        issue_secret: bool,
    ) -> Result<(OAuthClient, Option<String>)> {
        let client_id = Uuid::new_v4().to_string();
        let secret = if issue_secret {
            Some(random_token("svxcs_"))
        } else {
            None
        };
        let secret_hash = secret.as_deref().map(sha256_b64);
        let conn = self.conn.lock().map_err(|_| anyhow!("store poisoned"))?;
        conn.execute(
            "INSERT INTO oauth_clients (client_id, client_secret_hash, client_name, redirect_uris, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![client_id, secret_hash, client_name, serde_json::to_string(redirect_uris)?, now()],
        )?;
        Ok((
            OAuthClient {
                client_id,
                client_secret_hash: secret_hash,
                client_name: client_name.to_owned(),
                redirect_uris: redirect_uris.to_vec(),
            },
            secret,
        ))
    }

    pub fn get_client(&self, client_id: &str) -> Result<Option<OAuthClient>> {
        let conn = self.conn.lock().map_err(|_| anyhow!("store poisoned"))?;
        let row = conn
            .query_row(
                "SELECT client_id, client_secret_hash, client_name, redirect_uris FROM oauth_clients WHERE client_id = ?1",
                params![client_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        Ok(row.map(
            |(client_id, client_secret_hash, client_name, uris)| OAuthClient {
                client_id,
                client_secret_hash,
                client_name,
                redirect_uris: serde_json::from_str(&uris).unwrap_or_default(),
            },
        ))
    }

    // ---- grants ----------------------------------------------------------

    pub fn create_grant(
        &self,
        user_id: &str,
        api_key: &str,
        sarvaex_key_id: Option<&str>,
        can_trade: bool,
        label: &str,
    ) -> Result<String> {
        let grant_id = Uuid::new_v4().to_string();
        let enc = self.encrypt(api_key)?;
        let conn = self.conn.lock().map_err(|_| anyhow!("store poisoned"))?;
        conn.execute(
            "INSERT INTO grants (grant_id, user_id, api_key_enc, sarvaex_key_id, can_trade, label, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![grant_id, user_id, enc, sarvaex_key_id, can_trade as i64, label, now()],
        )?;
        Ok(grant_id)
    }

    pub fn get_grant(&self, grant_id: &str) -> Result<Option<Grant>> {
        let conn = self.conn.lock().map_err(|_| anyhow!("store poisoned"))?;
        let row = conn
            .query_row(
                "SELECT grant_id, user_id, api_key_enc, sarvaex_key_id, can_trade, label FROM grants WHERE grant_id = ?1 AND revoked_at IS NULL",
                params![grant_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Vec<u8>>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?;
        drop(conn);
        match row {
            None => Ok(None),
            Some((grant_id, user_id, enc, sarvaex_key_id, can_trade, label)) => {
                let api_key = self.decrypt(&enc)?;
                Ok(Some(Grant {
                    grant_id,
                    user_id,
                    api_key,
                    sarvaex_key_id,
                    can_trade: can_trade != 0,
                    label,
                }))
            }
        }
    }

    pub fn revoke_grant(&self, grant_id: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| anyhow!("store poisoned"))?;
        conn.execute(
            "UPDATE grants SET revoked_at = ?2 WHERE grant_id = ?1 AND revoked_at IS NULL",
            params![grant_id, now()],
        )?;
        conn.execute(
            "UPDATE oauth_tokens SET revoked_at = ?2 WHERE grant_id = ?1 AND revoked_at IS NULL",
            params![grant_id, now()],
        )?;
        Ok(())
    }

    // ---- authorization codes ------------------------------------------------

    pub fn create_code(
        &self,
        client_id: &str,
        redirect_uri: &str,
        code_challenge: &str,
        scope: &str,
        grant_id: &str,
    ) -> Result<String> {
        let code = random_token("svxac_");
        let conn = self.conn.lock().map_err(|_| anyhow!("store poisoned"))?;
        conn.execute(
            "INSERT INTO oauth_codes (code_hash, client_id, redirect_uri, code_challenge, scope, grant_id, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![sha256_b64(&code), client_id, redirect_uri, code_challenge, scope, grant_id, now() + AUTH_CODE_TTL_SECS],
        )?;
        Ok(code)
    }

    /// Consume a code exactly once. Returns `None` if unknown, expired or already used.
    pub fn consume_code(&self, code: &str) -> Result<Option<AuthCode>> {
        let hash = sha256_b64(code);
        let mut conn = self.conn.lock().map_err(|_| anyhow!("store poisoned"))?;
        let tx = conn.transaction()?;
        let row = tx
            .query_row(
                "SELECT client_id, redirect_uri, code_challenge, scope, grant_id, expires_at, used FROM oauth_codes WHERE code_hash = ?1",
                params![hash],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, i64>(5)?,
                        r.get::<_, i64>(6)?,
                    ))
                },
            )
            .optional()?;
        let Some((client_id, redirect_uri, code_challenge, scope, grant_id, expires_at, used)) =
            row
        else {
            return Ok(None);
        };
        tx.execute(
            "UPDATE oauth_codes SET used = 1 WHERE code_hash = ?1",
            params![hash],
        )?;
        tx.commit()?;
        if used != 0 || expires_at < now() {
            return Ok(None);
        }
        Ok(Some(AuthCode {
            client_id,
            redirect_uri,
            code_challenge,
            scope,
            grant_id,
            expires_at,
        }))
    }

    // ---- tokens ------------------------------------------------------------

    pub fn issue_tokens(
        &self,
        grant_id: &str,
        client_id: &str,
        scope: &str,
    ) -> Result<IssuedTokens> {
        let access = random_token("svxat_");
        let refresh = random_token("svxrt_");
        let conn = self.conn.lock().map_err(|_| anyhow!("store poisoned"))?;
        let n = now();
        conn.execute(
            "INSERT INTO oauth_tokens (token_hash, kind, grant_id, client_id, scope, expires_at) VALUES (?1, 'access', ?2, ?3, ?4, ?5)",
            params![sha256_b64(&access), grant_id, client_id, scope, n + ACCESS_TOKEN_TTL_SECS],
        )?;
        conn.execute(
            "INSERT INTO oauth_tokens (token_hash, kind, grant_id, client_id, scope, expires_at) VALUES (?1, 'refresh', ?2, ?3, ?4, ?5)",
            params![sha256_b64(&refresh), grant_id, client_id, scope, n + REFRESH_TOKEN_TTL_SECS],
        )?;
        Ok(IssuedTokens {
            access_token: access,
            refresh_token: refresh,
            expires_in: ACCESS_TOKEN_TTL_SECS,
            scope: scope.to_owned(),
        })
    }

    pub fn lookup_token(&self, token: &str, kind: &str) -> Result<Option<TokenRecord>> {
        let conn = self.conn.lock().map_err(|_| anyhow!("store poisoned"))?;
        let row = conn
            .query_row(
                "SELECT kind, grant_id, client_id, scope, expires_at FROM oauth_tokens WHERE token_hash = ?1 AND kind = ?2 AND revoked_at IS NULL AND expires_at > ?3",
                params![sha256_b64(token), kind, now()],
                |r| {
                    Ok(TokenRecord {
                        kind: r.get(0)?,
                        grant_id: r.get(1)?,
                        client_id: r.get(2)?,
                        scope: r.get(3)?,
                        expires_at: r.get(4)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Revoke one token (access or refresh). Returns the grant id if it was known.
    pub fn revoke_token(&self, token: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().map_err(|_| anyhow!("store poisoned"))?;
        let grant: Option<String> = conn
            .query_row(
                "SELECT grant_id FROM oauth_tokens WHERE token_hash = ?1",
                params![sha256_b64(token)],
                |r| r.get(0),
            )
            .optional()?;
        conn.execute(
            "UPDATE oauth_tokens SET revoked_at = ?2 WHERE token_hash = ?1",
            params![sha256_b64(token), now()],
        )?;
        Ok(grant)
    }

    /// Rotate: revoke the used refresh token and issue a fresh pair for the same grant.
    pub fn refresh(&self, refresh_token: &str, client_id: &str) -> Result<Option<IssuedTokens>> {
        let Some(rec) = self.lookup_token(refresh_token, "refresh")? else {
            return Ok(None);
        };
        if rec.client_id != client_id {
            return Ok(None);
        }
        self.revoke_token(refresh_token)?;
        Ok(Some(self.issue_tokens(
            &rec.grant_id,
            client_id,
            &rec.scope,
        )?))
    }

    /// Housekeeping: drop expired codes and tokens.
    pub fn purge_expired(&self) -> Result<usize> {
        let conn = self.conn.lock().map_err(|_| anyhow!("store poisoned"))?;
        let n = now();
        let a = conn.execute("DELETE FROM oauth_codes WHERE expires_at < ?1", params![n])?;
        let b = conn.execute(
            "DELETE FROM oauth_tokens WHERE expires_at < ?1",
            params![n - 86_400],
        )?;
        Ok(a + b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::in_memory([7u8; 32]).unwrap()
    }

    #[test]
    fn roundtrip_grant_and_tokens() {
        let s = store();
        let (client, secret) = s
            .register_client(
                "Claude",
                &["https://claude.ai/api/mcp/auth_callback".into()],
                true,
            )
            .unwrap();
        assert!(secret.is_some());
        assert_eq!(
            s.get_client(&client.client_id)
                .unwrap()
                .unwrap()
                .client_name,
            "Claude"
        );

        let grant = s
            .create_grant("alice", "svx_live_secret", Some("kid"), true, "test")
            .unwrap();
        let g = s.get_grant(&grant).unwrap().unwrap();
        assert_eq!(g.api_key, "svx_live_secret");
        assert!(g.can_trade);

        let code = s
            .create_code(
                &client.client_id,
                "https://claude.ai/cb",
                "chal",
                "read trade",
                &grant,
            )
            .unwrap();
        let c = s.consume_code(&code).unwrap().unwrap();
        assert_eq!(c.grant_id, grant);
        assert!(
            s.consume_code(&code).unwrap().is_none(),
            "codes are single-use"
        );

        let t = s
            .issue_tokens(&grant, &client.client_id, "read trade")
            .unwrap();
        assert!(s.lookup_token(&t.access_token, "access").unwrap().is_some());
        assert!(
            s.lookup_token(&t.access_token, "refresh")
                .unwrap()
                .is_none()
        );
        let t2 = s
            .refresh(&t.refresh_token, &client.client_id)
            .unwrap()
            .unwrap();
        assert!(
            s.lookup_token(&t.refresh_token, "refresh")
                .unwrap()
                .is_none(),
            "old refresh revoked"
        );
        assert!(
            s.lookup_token(&t2.access_token, "access")
                .unwrap()
                .is_some()
        );

        s.revoke_grant(&grant).unwrap();
        assert!(s.get_grant(&grant).unwrap().is_none());
        assert!(
            s.lookup_token(&t2.access_token, "access")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn pkce() {
        // RFC 7636 appendix B test vector.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert!(pkce_matches(
            verifier,
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        ));
        assert!(!pkce_matches(
            "nope",
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        ));
    }

    #[test]
    fn secret_parsing() {
        let hex = "00".repeat(32);
        assert_eq!(parse_secret(&hex).unwrap(), [0u8; 32]);
        let b64 = STANDARD.encode([9u8; 32]);
        assert_eq!(parse_secret(&b64).unwrap(), [9u8; 32]);
        assert!(parse_secret("short").is_err());
    }
}
