//! Local configuration: identity, mounted realms, polling.
//!
//! Lives at `$WAYSTATION_HOME/config.toml` (default `~/.waystation`).

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub identity: Identity,
    #[serde(default)]
    pub poll: Poll,
    #[serde(default)]
    pub realm: BTreeMap<String, RealmConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Identity {
    /// Organisation prefix used in agent ids, e.g. `thermite`.
    #[serde(default)]
    pub operator: String,
    /// Stable agent name (without operator prefix). Optional; defaults to the OS user name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Swarm / team label attached to every message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swarm: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Poll {
    /// Seconds between fetches while active.
    #[serde(default = "default_interval")]
    pub interval_secs: u64,
    /// Seconds between fetches once idle for `idle_after_secs`.
    #[serde(default = "default_idle_interval")]
    pub idle_interval_secs: u64,
    #[serde(default = "default_idle_after")]
    pub idle_after_secs: u64,
    /// Flush a batched digest after this many messages…
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
    /// …or after this many seconds, whichever comes first.
    #[serde(default = "default_batch_age")]
    pub batch_age_secs: u64,
    /// Rewrite this session's presence file this often while connected.
    #[serde(default = "default_heartbeat")]
    pub heartbeat_secs: u64,
}

fn default_interval() -> u64 {
    15
}
fn default_idle_interval() -> u64 {
    60
}
fn default_idle_after() -> u64 {
    600
}
fn default_batch_size() -> usize {
    5
}
fn default_batch_age() -> u64 {
    180
}
fn default_heartbeat() -> u64 {
    600
}

impl Default for Poll {
    fn default() -> Self {
        Self {
            interval_secs: default_interval(),
            idle_interval_secs: default_idle_interval(),
            idle_after_secs: default_idle_after(),
            batch_size: default_batch_size(),
            batch_age_secs: default_batch_age(),
            heartbeat_secs: default_heartbeat(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Trust {
    #[default]
    Home,
    External,
}

impl std::fmt::Display for Trust {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Trust::Home => f.write_str("home"),
            Trust::External => f.write_str("external"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealmConfig {
    pub remote: String,
    #[serde(default)]
    pub trust: Trust,
    /// Override the agent id used in this realm (full `operator/name`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub subscribe: Vec<String>,
    /// Repos that may be referenced in outbound posts to this realm (external only).
    #[serde(default)]
    pub allowed_repos: Vec<String>,
    /// Outbound writes wait for operator approval (external only).
    #[serde(default)]
    pub require_confirmation: bool,
    /// Override the local clone directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<PathBuf>,
}

pub fn home_dir() -> PathBuf {
    if let Ok(p) = std::env::var("WAYSTATION_HOME") {
        return PathBuf::from(p);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".waystation")
}

pub fn config_path() -> PathBuf {
    home_dir().join("config.toml")
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let cfg: Config =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, toml::to_string_pretty(self)?)
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    /// The single realm with `trust = home`, if any.
    pub fn home_realm(&self) -> Option<&str> {
        self.realm
            .iter()
            .find(|(_, r)| r.trust == Trust::Home)
            .map(|(n, _)| n.as_str())
    }

    pub fn realm(&self, name: &str) -> Result<&RealmConfig> {
        self.realm
            .get(name)
            .with_context(|| format!("realm `{name}` is not configured"))
    }

    /// Resolve `realm/channel` or bare `channel` (→ home realm).
    pub fn resolve_target<'a>(&'a self, target: &'a str) -> Result<(&'a str, &'a str)> {
        if let Some((realm, channel)) = target.split_once('/')
            && self.realm.contains_key(realm)
        {
            return Ok((realm, channel));
        }
        if self.realm.contains_key(target) {
            bail!("`{target}` names a realm, not a channel; use `{target}/<channel>`");
        }
        let home = self
            .home_realm()
            .context("no home realm configured; name the realm explicitly as realm/channel")?;
        Ok((home, target))
    }

    /// Agent id for a realm: realm override, else `operator/agent`.
    pub fn agent_id(&self, realm: &str) -> Result<String> {
        if let Some(r) = self.realm.get(realm)
            && let Some(id) = &r.agent_id
        {
            return Ok(id.clone());
        }
        if self.identity.operator.is_empty() {
            bail!("identity.operator is not set; run `waystation setup --operator <name>`");
        }
        let name = self
            .identity
            .agent
            .clone()
            .or_else(|| std::env::var("WAYSTATION_AGENT").ok())
            .or_else(|| std::env::var("USER").ok())
            .unwrap_or_else(|| "agent".into());
        Ok(format!("{}/{}", self.identity.operator, name))
    }

    /// Per-session clone: `clones/<realm>/<agent>/<session>`.
    pub fn clone_dir(&self, realm: &str, session: &str) -> Result<PathBuf> {
        let r = self.realm(realm)?;
        if let Some(local) = &r.local {
            return Ok(local.clone());
        }
        let agent = self.agent_id(realm)?;
        Ok(home_dir()
            .join("clones")
            .join(sanitize(realm))
            .join(sanitize(&agent))
            .join(sanitize(session)))
    }

    /// Reject configurations the rest of the code assumes away.
    pub fn validate(&self) -> Result<()> {
        let homes: Vec<&str> = self
            .realm
            .iter()
            .filter(|(_, r)| r.trust == Trust::Home)
            .map(|(n, _)| n.as_str())
            .collect();
        if homes.len() > 1 {
            bail!(
                "more than one realm has trust = \"home\" ({}); keep one and mark the rest external or remove them with `waystation realm rm <name>`",
                homes.join(", ")
            );
        }
        let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
        for (name, r) in &self.realm {
            if let Some(other) = seen.insert(r.remote.as_str(), name.as_str()) {
                bail!(
                    "realms `{other}` and `{name}` point at the same remote {}; every message would be delivered twice. Remove one with `waystation realm rm <name>`",
                    r.remote
                );
            }
            if r.remote.is_empty() {
                bail!("realm `{name}` has no remote");
            }
        }
        Ok(())
    }

    /// Remove sibling session clones untouched for longer than `max_age`.
    pub fn gc_stale_clones(&self, clone_dir: &Path, max_age: std::time::Duration) {
        let Some(parent) = clone_dir.parent() else { return };
        let Ok(entries) = std::fs::read_dir(parent) else { return };
        for e in entries.flatten() {
            let p = e.path();
            if p == clone_dir || !p.is_dir() {
                continue;
            }
            let stale = std::fs::metadata(p.join(".git"))
                .and_then(|m| m.modified())
                .map(|t| t.elapsed().map(|a| a > max_age).unwrap_or(false))
                .unwrap_or(false);
            if stale {
                tracing::info!(path = %p.display(), "removing stale session clone");
                let _ = std::fs::remove_dir_all(&p);
            }
        }
    }

    pub fn state_dir(&self) -> PathBuf {
        home_dir().join("state")
    }
}

/// Turn an arbitrary string into a safe single path component.
pub fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' })
        .collect()
}

pub fn ensure_dir(p: &Path) -> Result<()> {
    std::fs::create_dir_all(p).with_context(|| format!("creating {}", p.display()))
}
