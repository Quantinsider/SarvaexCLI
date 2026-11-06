//! Idempotency-Key and client_order_id generation.
//!
//! Every mutating gateway call needs a unique `Idempotency-Key` scoped to
//! (user, method, path). Retrying with the **same** key returns the stored
//! result; reusing a key with a different body is a `409 IDEMPOTENCY_KEY_REUSED`.

use uuid::Uuid;

/// A fresh idempotency key: `<prefix>-<yyyymmddThhmmss>-<uuid>`.
pub fn idempotency_key(prefix: &str) -> String {
    let ts = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    format!("{prefix}-{ts}-{}", Uuid::new_v4().simple())
}

/// A fresh client order id (`cli-<uuid>`), at most 128 characters as the gateway requires.
pub fn client_order_id() -> String {
    format!("cli-{}", Uuid::new_v4().simple())
}

/// Deterministic idempotency key derived from a client-chosen id, so a caller
/// that retries `place --client-order-id X` across processes reuses the same key.
pub fn idempotency_key_for(prefix: &str, client_id: &str) -> String {
    format!("{prefix}-{}", sanitize(client_id))
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .take(200)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_unique_and_bounded() {
        let a = idempotency_key("order");
        let b = idempotency_key("order");
        assert_ne!(a, b);
        assert!(a.starts_with("order-"));
        assert!(client_order_id().len() <= 128);
    }

    #[test]
    fn deterministic_key_from_client_id() {
        assert_eq!(
            idempotency_key_for("order", "abc/def gh"),
            "order-abc_def_gh"
        );
        assert_eq!(
            idempotency_key_for("order", "x"),
            idempotency_key_for("order", "x")
        );
    }
}
