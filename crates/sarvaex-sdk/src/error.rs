//! Error type for the SDK.
//!
//! API errors preserve the gateway's stable `{ "error": { "code", "message" } }`
//! envelope so callers (and agents) can branch on `code` rather than parsing text.

use serde::{Deserialize, Serialize};

pub type Result<T> = std::result::Result<T, SarvaexError>;

#[derive(Debug, thiserror::Error)]
pub enum SarvaexError {
    /// The gateway returned an error envelope.
    #[error("{code} ({status}): {message}")]
    Api {
        status: u16,
        code: String,
        message: String,
        /// Seconds the server asked us to wait (from `Retry-After`), if any.
        retry_after: Option<u64>,
    },

    /// No credential configured for an authenticated endpoint.
    #[error("not authenticated: {0}")]
    Unauthenticated(String),

    /// Client-side validation failed before any request was sent.
    #[error("{0}")]
    Validation(String),

    /// The request was blocked by a local safety guard (read-only mode, notional cap, allowlist).
    #[error("blocked by safety guard: {0}")]
    Guard(String),

    #[error("network error: {0}")]
    Transport(#[from] reqwest::Error),

    #[error("websocket error: {0}")]
    WebSocket(String),

    #[error("could not decode response from {endpoint}: {source}; body: {body}")]
    Decode {
        endpoint: String,
        #[source]
        source: serde_json::Error,
        body: String,
    },

    #[error("{0}")]
    Other(String),
}

impl SarvaexError {
    pub fn api(status: u16, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Api {
            status,
            code: code.into(),
            message: message.into(),
            retry_after: None,
        }
    }

    /// Stable machine-readable code for this error.
    pub fn code(&self) -> &str {
        match self {
            Self::Api { code, .. } => code,
            Self::Unauthenticated(_) => "UNAUTHENTICATED",
            Self::Validation(_) => "VALIDATION",
            Self::Guard(_) => "GUARD",
            Self::Transport(_) => "TRANSPORT",
            Self::WebSocket(_) => "WEBSOCKET",
            Self::Decode { .. } => "DECODE",
            Self::Other(_) => "ERROR",
        }
    }

    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Api { status, .. } => Some(*status),
            _ => None,
        }
    }

    pub fn is_rate_limited(&self) -> bool {
        matches!(self, Self::Api { status: 429, .. })
    }

    pub fn is_unauthenticated(&self) -> bool {
        matches!(
            self,
            Self::Api { status: 401, .. } | Self::Unauthenticated(_)
        )
    }

    pub fn is_forbidden(&self) -> bool {
        matches!(self, Self::Api { status: 403, .. })
    }

    /// Whether retrying the identical request (same idempotency key) is sensible.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Api { status, code, .. } => {
                matches!(status, 429 | 502 | 503 | 504)
                    || code == "OPERATION_IN_PROGRESS"
                    || code == "IDEMPOTENCY_STORE_UNAVAILABLE"
            }
            Self::Transport(e) => e.is_timeout() || e.is_connect() || e.is_request(),
            _ => false,
        }
    }

    /// JSON representation suitable for `--json` output and MCP tool errors.
    pub fn to_json(&self) -> serde_json::Value {
        let message = match self {
            Self::Api { message, .. } => message.clone(),
            other => other.to_string(),
        };
        let mut v = serde_json::json!({
            "error": { "code": self.code(), "message": message }
        });
        if let Some(status) = self.status() {
            v["error"]["http_status"] = serde_json::json!(status);
        }
        if let Self::Api {
            retry_after: Some(s),
            ..
        } = self
        {
            v["error"]["retry_after_seconds"] = serde_json::json!(s);
        }
        v
    }
}

/// Wire shape of the gateway error envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorEnvelope {
    pub error: ErrorBody,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub message: String,
}
