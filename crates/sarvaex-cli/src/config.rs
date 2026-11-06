//! Profiles live in `~/.config/sarvaex/config.toml` (override with
//! `SARVAEX_CONFIG`). Environment variables and flags take precedence.

use anyhow::{Context, Result, anyhow};
use sarvaex_sdk::{Client, ClientConfig, Credential, Guards, price};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub base_url: Option<String>,
    pub ws_url: Option<String>,
    pub api_key: Option<String>,
    pub user_id: Option<String>,
    pub key_prefix: Option<String>,
    pub key_id: Option<String>,
    pub read_only: Option<bool>,
    pub max_order_notional_usdc: Option<String>,
    pub max_order_count: Option<i64>,
    pub allowed_tickers: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ConfigFile {
    pub default_profile: Option<String>,
    pub profiles: BTreeMap<String, Profile>,
}

pub fn config_dir() -> PathBuf {
    if let Ok(p) = std::env::var("SARVAEX_CONFIG") {
        return PathBuf::from(p)
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
    }
    directories::ProjectDirs::from("com", "sarvaex", "sarvaex")
        .map(|d| d.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".sarvaex"))
}

pub fn config_path() -> PathBuf {
    if let Ok(p) = std::env::var("SARVAEX_CONFIG") {
        return PathBuf::from(p);
    }
    config_dir().join("config.toml")
}

impl ConfigFile {
    pub fn load() -> Result<Self> {
        let path = config_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(self)?;
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    pub fn profile_name(&self, requested: Option<&str>) -> String {
        requested
            .map(str::to_owned)
            .or_else(|| self.default_profile.clone())
            .unwrap_or_else(|| "default".into())
    }

    pub fn profile(&self, name: &str) -> Profile {
        self.profiles.get(name).cloned().unwrap_or_default()
    }

    pub fn profile_mut(&mut self, name: &str) -> &mut Profile {
        if self.default_profile.is_none() {
            self.default_profile = Some(name.to_owned());
        }
        self.profiles.entry(name.to_owned()).or_default()
    }
}

/// Everything needed to build a client for one invocation.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub profile_name: String,
    pub profile: Profile,
    pub base_url: String,
    pub ws_url: Option<String>,
    pub credential: Credential,
    pub guards: Guards,
    pub timeout: Duration,
}

impl Resolved {
    pub fn from_cli(g: &crate::cli::GlobalOpts) -> Result<Self> {
        let file = ConfigFile::load()?;
        // Empty environment variables (`SARVAEX_API_KEY=`) count as unset.
        let non_empty = |v: &Option<String>| {
            v.as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        let g_profile = non_empty(&g.profile);
        let g_base_url = non_empty(&g.base_url);
        let g_ws_url = non_empty(&g.ws_url);
        let g_api_key = non_empty(&g.api_key);
        let g_token = non_empty(&g.token);
        let profile_name = file.profile_name(g_profile.as_deref());
        let profile = file.profile(&profile_name);

        let base_url = g_base_url
            .clone()
            .or_else(|| profile.base_url.clone())
            .unwrap_or_else(|| sarvaex_sdk::DEFAULT_BASE_URL.to_owned());
        let base_url = match base_url.as_str() {
            "local" | "localhost" | "docker" => sarvaex_sdk::LOCAL_BASE_URL.to_owned(),
            "prod" | "production" => sarvaex_sdk::DEFAULT_BASE_URL.to_owned(),
            _ => base_url,
        };
        let ws_url = g_ws_url.or_else(|| profile.ws_url.clone());

        let credential = if let Some(k) = g_api_key {
            Credential::ApiKey(k)
        } else if let Some(t) = g_token {
            Credential::Bearer(t)
        } else if let Some(k) = &profile.api_key {
            Credential::from_secret(k.clone())
        } else {
            Credential::None
        };

        let env_bool = |name: &str| {
            std::env::var(name).ok().map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
        };
        let read_only = env_bool("SARVAEX_READ_ONLY")
            .or(profile.read_only)
            .unwrap_or(false);
        let max_notional = std::env::var("SARVAEX_MAX_ORDER_NOTIONAL_USDC")
            .ok()
            .or_else(|| profile.max_order_notional_usdc.clone());
        let max_order_notional_micro = match max_notional {
            Some(v) if !v.trim().is_empty() => Some(
                price::usdc_to_micro(&v)
                    .map_err(|e| anyhow!("SARVAEX_MAX_ORDER_NOTIONAL_USDC: {e}"))?,
            ),
            _ => None,
        };
        let max_order_count = std::env::var("SARVAEX_MAX_ORDER_COUNT")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .or(profile.max_order_count);
        let allowed_tickers = std::env::var("SARVAEX_ALLOWED_TICKERS")
            .ok()
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
            })
            .or_else(|| profile.allowed_tickers.clone())
            .filter(|v| !v.is_empty());

        Ok(Self {
            profile_name,
            profile,
            base_url,
            ws_url,
            credential,
            guards: Guards {
                read_only,
                max_order_notional_micro,
                max_order_count,
                allowed_tickers,
            },
            timeout: Duration::from_secs(g.timeout.max(1)),
        })
    }

    pub fn client(&self) -> Result<Client> {
        let mut cfg = ClientConfig::new(&self.base_url)?
            .with_credential(self.credential.clone())
            .with_timeout(self.timeout)
            .with_guards(self.guards.clone())
            .with_user_agent(format!("sarvaex-cli/{}", env!("CARGO_PKG_VERSION")));
        if let Some(ws) = &self.ws_url {
            cfg = cfg.with_ws_url(ws)?;
        }
        Ok(Client::new(cfg)?)
    }

    pub fn guards_summary(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.guards.read_only {
            out.push("read-only".into());
        }
        if let Some(m) = self.guards.max_order_notional_micro {
            out.push(format!(
                "max order notional {} USDC",
                price::micro_to_usdc(m)
            ));
        }
        if let Some(c) = self.guards.max_order_count {
            out.push(format!("max order count {c}"));
        }
        if let Some(t) = &self.guards.allowed_tickers {
            out.push(format!("allowed tickers: {}", t.join(",")));
        }
        out
    }
}

/// Persist a credential into a profile.
pub fn store_credential(
    profile_name: &str,
    base_url: &str,
    api_key: &str,
    user_id: Option<&str>,
    key_id: Option<&str>,
) -> Result<PathBuf> {
    let mut file = ConfigFile::load()?;
    let p = file.profile_mut(profile_name);
    p.base_url = Some(base_url.to_owned());
    p.api_key = Some(api_key.to_owned());
    p.key_prefix = Some(api_key.chars().take(17).collect());
    if let Some(u) = user_id {
        p.user_id = Some(u.to_owned());
    }
    p.key_id = key_id.map(str::to_owned);
    file.save()?;
    Ok(config_path())
}

pub fn clear_credential(profile_name: &str) -> Result<Option<Profile>> {
    let mut file = ConfigFile::load()?;
    let old = file.profiles.get(profile_name).cloned();
    if let Some(p) = file.profiles.get_mut(profile_name) {
        p.api_key = None;
        p.key_prefix = None;
        p.key_id = None;
        file.save()?;
    }
    Ok(old)
}
