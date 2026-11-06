//! Output formatting. JSON when `--json` or stdout is not a TTY; tables otherwise.

use comfy_table::{ContentArrangement, Table, presets::UTF8_FULL_CONDENSED};
use sarvaex_sdk::SarvaexError;
use serde::Serialize;
use std::io::IsTerminal;

pub mod exit {
    pub const OK: i32 = 0;
    pub const ERROR: i32 = 1;
    pub const USAGE: i32 = 2;
    pub const AUTH: i32 = 3;
    pub const REJECTED: i32 = 4;
    pub const RATE_LIMITED: i32 = 5;
    pub const NETWORK: i32 = 6;
    pub const GUARD: i32 = 7;
}

#[derive(Debug, Clone, Copy)]
pub struct Output {
    pub json: bool,
    pub compact: bool,
    pub tty: bool,
}

impl Output {
    pub fn new(force_json: bool, compact: bool) -> Self {
        let tty = std::io::stdout().is_terminal();
        Self {
            json: force_json || !tty,
            compact,
            tty,
        }
    }

    pub fn interactive(&self) -> bool {
        self.tty && std::io::stdin().is_terminal()
    }

    /// Emit a value: JSON in json mode, otherwise the provided human rendering.
    pub fn emit<T: Serialize>(&self, value: &T, human: impl FnOnce() -> String) {
        if self.json {
            self.json_out(value);
        } else {
            let s = human();
            if !s.is_empty() {
                println!("{s}");
            }
        }
    }

    pub fn json_out<T: Serialize>(&self, value: &T) {
        let s = if self.compact {
            serde_json::to_string(value)
        } else {
            serde_json::to_string_pretty(value)
        };
        match s {
            Ok(s) => println!("{s}"),
            Err(e) => eprintln!("{{\"error\":{{\"code\":\"SERIALIZE\",\"message\":\"{e}\"}}}}"),
        }
    }

    /// Informational line (stderr in json mode so stdout stays machine-clean).
    #[allow(dead_code)]
    pub fn note(&self, msg: &str) {
        if self.json {
            eprintln!("{msg}");
        } else {
            println!("{msg}");
        }
    }

    pub fn error(&self, err: &anyhow::Error) {
        let v = match err.downcast_ref::<SarvaexError>() {
            Some(e) => e.to_json(),
            None => {
                serde_json::json!({ "error": { "code": "ERROR", "message": format!("{err:#}") } })
            }
        };
        if self.json {
            eprintln!(
                "{}",
                if self.compact {
                    v.to_string()
                } else {
                    serde_json::to_string_pretty(&v).unwrap_or_default()
                }
            );
        } else {
            let code = v["error"]["code"].as_str().unwrap_or("ERROR");
            let msg = v["error"]["message"].as_str().unwrap_or("");
            eprintln!("error [{code}]: {msg}");
            if let Some(s) = v["error"]["retry_after_seconds"].as_u64() {
                eprintln!("retry after {s}s");
            }
        }
    }
}

pub fn exit_code_for(err: &anyhow::Error) -> i32 {
    match err.downcast_ref::<SarvaexError>() {
        Some(e) if e.is_unauthenticated() || e.is_forbidden() => exit::AUTH,
        Some(e) if e.is_rate_limited() => exit::RATE_LIMITED,
        Some(SarvaexError::Transport(_)) | Some(SarvaexError::WebSocket(_)) => exit::NETWORK,
        Some(SarvaexError::Guard(_)) => exit::GUARD,
        Some(SarvaexError::Validation(_)) => exit::USAGE,
        _ => exit::ERROR,
    }
}

pub fn table(headers: &[&str]) -> Table {
    let mut t = Table::new();
    t.load_style(UTF8_FULL_CONDENSED)
        .set_content_arrangement(ContentArrangement::Dynamic);
    // Terminal width detection fails under some PTYs (CI, `script`, IDE consoles) and
    // comfy-table then squeezes every column to one character; fall back to a sane width.
    if t.width().is_none_or(|w| w < 60) {
        let cols = std::env::var("COLUMNS")
            .ok()
            .and_then(|c| c.parse::<u16>().ok())
            .filter(|c| *c >= 60)
            .unwrap_or(140);
        t.set_width(cols);
    }
    t.set_header(headers.iter().map(|h| h.to_string()).collect::<Vec<_>>());
    t
}

pub fn kv(rows: &[(&str, String)]) -> String {
    let w = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    rows.iter()
        .map(|(k, v)| format!("{k:<w$}  {v}"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn short_ts(ts: &Option<String>) -> String {
    ts.as_deref()
        .map(|s| s.chars().take(19).collect::<String>().replace('T', " "))
        .unwrap_or_default()
}

pub fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_owned()
    } else {
        format!(
            "{}…",
            s.chars().take(n.saturating_sub(1)).collect::<String>()
        )
    }
}
