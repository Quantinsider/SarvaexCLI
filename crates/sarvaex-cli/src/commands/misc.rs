use super::Ctx;
use crate::cli::{ApiArgs, ConfigCmd, DocsCmd, GlobalOpts, MarketRequestsCmd};
use crate::config::{self, ConfigFile};
use crate::output::{Output, exit, kv};
use anyhow::{Context, Result, bail};
use sarvaex_sdk::NewMarketRequest;
use serde_json::{Value, json};

pub fn docs(out: &Output, cmd: &DocsCmd) -> Result<i32> {
    match cmd {
        DocsCmd::Skill => {
            println!("{}", sarvaex_mcp::SKILL_MD);
            Ok(exit::OK)
        }
        DocsCmd::Install { dir } => {
            let root = std::path::Path::new(dir);
            let skill_dir = root.join(".claude").join("skills").join("sarvaex");
            std::fs::create_dir_all(&skill_dir)?;
            let skill_path = skill_dir.join("SKILL.md");
            std::fs::write(&skill_path, sarvaex_mcp::SKILL_MD)?;

            // Append a pointer to AGENTS.md (used by Codex and others) without clobbering it.
            let agents = root.join("AGENTS.md");
            let marker = "## SarvaEX trading";
            let section = format!(
                "\n{marker}\n\nUse the `sarvaex` CLI to trade on SarvaEX. Read `.claude/skills/sarvaex/SKILL.md` first; \
                 run `sarvaex docs` for the same content. Always add `--json`, check `status`/`reject_code` in order responses, \
                 and never retry an order without reusing its `--client-order-id`.\n"
            );
            let mut wrote_agents = false;
            let existing = std::fs::read_to_string(&agents).unwrap_or_default();
            if !existing.contains(marker) {
                std::fs::write(&agents, format!("{existing}{section}"))?;
                wrote_agents = true;
            }
            out.emit(&json!({ "skill": skill_path, "agents_md": agents, "agents_md_updated": wrote_agents }), || {
                format!("wrote {}\n{} {}", skill_path.display(), if wrote_agents { "updated" } else { "already up to date:" }, agents.display())
            });
            Ok(exit::OK)
        }
    }
}

pub fn config(out: &Output, g: &GlobalOpts, cmd: &ConfigCmd) -> Result<i32> {
    match cmd {
        ConfigCmd::Path => {
            let p = config::config_path();
            out.emit(&json!({ "path": p }), || p.display().to_string());
        }
        ConfigCmd::Show => {
            let file = ConfigFile::load()?;
            let active = file.profile_name(g.profile.as_deref());
            let mut redacted = file.clone();
            for p in redacted.profiles.values_mut() {
                if let Some(k) = &p.api_key {
                    p.api_key = Some(format!("{}…", k.chars().take(17).collect::<String>()));
                }
            }
            out.emit(&json!({ "path": config::config_path(), "active_profile": active, "config": redacted }), || {
                let mut s = format!("config: {}\nactive profile: {active}\n", config::config_path().display());
                for (name, p) in &redacted.profiles {
                    s.push_str(&format!("\n[{name}]\n"));
                    s.push_str(&kv(&[
                        ("base_url", p.base_url.clone().unwrap_or_default()),
                        ("api_key", p.api_key.clone().unwrap_or_else(|| "(none)".into())),
                        ("user_id", p.user_id.clone().unwrap_or_default()),
                        ("read_only", p.read_only.map(|b| b.to_string()).unwrap_or_default()),
                        ("max_order_notional_usdc", p.max_order_notional_usdc.clone().unwrap_or_default()),
                        ("allowed_tickers", p.allowed_tickers.clone().map(|t| t.join(",")).unwrap_or_default()),
                    ]));
                    s.push('\n');
                }
                s
            });
        }
        ConfigCmd::Use { profile } => {
            let mut file = ConfigFile::load()?;
            file.profile_mut(profile);
            file.default_profile = Some(profile.clone());
            file.save()?;
            out.emit(&json!({ "ok": true, "default_profile": profile }), || {
                format!("default profile is now '{profile}'")
            });
        }
        ConfigCmd::Set { key, value } => {
            let mut file = ConfigFile::load()?;
            let name = file.profile_name(g.profile.as_deref());
            let p = file.profile_mut(&name);
            match key.as_str() {
                "base_url" => p.base_url = Some(value.clone()),
                "ws_url" => p.ws_url = Some(value.clone()),
                "user_id" => p.user_id = Some(value.clone()),
                "read_only" => {
                    p.read_only = Some(matches!(
                        value.to_ascii_lowercase().as_str(),
                        "1" | "true" | "yes" | "on"
                    ))
                }
                "max_order_notional_usdc" => {
                    sarvaex_sdk::price::usdc_to_micro(value)?;
                    p.max_order_notional_usdc = Some(value.clone());
                }
                "max_order_count" => {
                    p.max_order_count = Some(
                        value
                            .parse()
                            .context("max_order_count must be an integer")?,
                    )
                }
                "allowed_tickers" => {
                    p.allowed_tickers = Some(
                        value
                            .split(',')
                            .map(|s| s.trim().to_ascii_uppercase())
                            .filter(|s| !s.is_empty())
                            .collect(),
                    )
                }
                other => bail!("unknown key '{other}'"),
            }
            file.save()?;
            out.emit(
                &json!({ "ok": true, "profile": name, "key": key, "value": value }),
                || format!("set {key} in profile '{name}'"),
            );
        }
        ConfigCmd::Delete { profile } => {
            let mut file = ConfigFile::load()?;
            let removed = file.profiles.remove(profile).is_some();
            if file.default_profile.as_deref() == Some(profile) {
                file.default_profile = None;
            }
            file.save()?;
            out.emit(&json!({ "ok": removed, "profile": profile }), || {
                if removed {
                    format!("deleted profile '{profile}'")
                } else {
                    format!("no profile '{profile}'")
                }
            });
        }
    }
    Ok(exit::OK)
}

pub async fn health(ctx: &Ctx) -> Result<i32> {
    let v = ctx.client().health_overview().await?;
    ctx.out.emit(&v, || {
        let summary = &v["summary"];
        let mut s = format!(
            "{}/{} services running",
            summary["running"], summary["total"]
        );
        if let Some(items) = v["items"].as_array() {
            for it in items {
                s.push_str(&format!(
                    "\n  {:<18} {:<10} {}",
                    it["name"].as_str().unwrap_or(""),
                    it["status"].as_str().unwrap_or(""),
                    it["detail"].as_str().unwrap_or("")
                ));
            }
        }
        s
    });
    Ok(exit::OK)
}

pub async fn api(ctx: &Ctx, args: ApiArgs) -> Result<i32> {
    let method: reqwest::Method = args
        .method
        .to_ascii_uppercase()
        .parse()
        .context("invalid HTTP method")?;
    let (path, query) = match args.path.split_once('?') {
        Some((p, q)) => (p.to_owned(), q.to_owned()),
        None => (args.path.clone(), String::new()),
    };
    let pairs: Vec<(String, String)> = url::form_urlencoded::parse(query.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let q: Vec<(&str, &str)> = pairs
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let body: Option<Value> = match &args.body {
        Some(b) if b.starts_with('@') => {
            Some(serde_json::from_str(&std::fs::read_to_string(&b[1..])?)?)
        }
        Some(b) => Some(serde_json::from_str(b).context("--body must be JSON")?),
        None if method != reqwest::Method::GET => Some(json!({})),
        None => None,
    };
    let idem = match (&args.idempotency_key, method == reqwest::Method::GET) {
        (Some(k), _) => Some(k.clone()),
        (None, false) => Some(sarvaex_sdk::idempotency::idempotency_key("api")),
        (None, true) => None,
    };
    let v = ctx
        .client()
        .raw(method, &path, &q, body.as_ref(), idem.as_deref())
        .await?;
    ctx.out.json_out(&v);
    Ok(exit::OK)
}

pub async fn market_requests(ctx: &Ctx, cmd: MarketRequestsCmd) -> Result<i32> {
    match cmd {
        MarketRequestsCmd::List => {
            let v = ctx.client().list_market_requests().await?;
            ctx.out
                .emit(&v, || serde_json::to_string_pretty(&v).unwrap_or_default());
        }
        MarketRequestsCmd::Create {
            question,
            kind,
            category,
            source,
        } => {
            let req = NewMarketRequest {
                question,
                kind: kind.to_ascii_uppercase(),
                category,
                source,
            };
            let v = ctx.client().create_market_request(&req).await?;
            ctx.out
                .emit(&v, || serde_json::to_string_pretty(&v).unwrap_or_default());
        }
    }
    Ok(exit::OK)
}
