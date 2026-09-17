//! Per-agent inbox: persistent cursors, unread queue, and push-tier classification.

use crate::config::{Config, Trust, ensure_dir, sanitize};
use crate::model::{Kind, Message, Priority};
use crate::poller::Event;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Immediate,
    Batched,
    Silent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Unread {
    pub realm: String,
    pub trust: Trust,
    pub tier: Tier,
    pub message: Message,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct InboxState {
    /// realm → last seen remote head
    #[serde(default)]
    pub cursors: BTreeMap<String, String>,
    #[serde(default)]
    pub unread: Vec<Unread>,
    /// ids of threads this agent started (for reply routing)
    #[serde(default)]
    pub my_threads: BTreeSet<String>,
    #[serde(skip)]
    path: PathBuf,
}

impl InboxState {
    pub fn load(cfg: &Config, agent_key: &str) -> Result<Self> {
        let dir = cfg.state_dir();
        ensure_dir(&dir)?;
        let path = dir.join(format!("{}.json", sanitize(agent_key)));
        let mut st: InboxState = if path.exists() {
            let text = std::fs::read_to_string(&path)?;
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?
        } else {
            InboxState::default()
        };
        st.path = path;
        Ok(st)
    }

    pub fn save(&self) -> Result<()> {
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    pub fn cursor(&self, realm: &str) -> Option<&str> {
        self.cursors.get(realm).map(String::as_str)
    }

    pub fn set_cursor(&mut self, realm: &str, head: String) {
        self.cursors.insert(realm.to_string(), head);
    }

    /// Classify and queue events. Returns the newly queued unread items.
    pub fn ingest(
        &mut self,
        events: Vec<Event>,
        me: &BTreeMap<String, (String, Trust, Vec<String>)>,
        my_session: &str,
    ) -> Vec<Unread> {
        let mut added = Vec::new();
        for ev in events {
            let Event::Message { realm, message, .. } = ev else { continue };
            let Some((agent_id, trust, subs)) = me.get(&realm) else { continue };
            // Skip only what *this session* wrote; another session of the same agent is a peer.
            if message.from.agent == *agent_id && message.from.session.as_deref() == Some(my_session) {
                continue;
            }
            let tier = classify(&message, agent_id, subs, &self.my_threads);
            let u = Unread { realm, trust: *trust, tier, message };
            self.unread.push(u.clone());
            added.push(u);
        }
        added
    }

    /// Remove and return unread items, optionally filtered.
    pub fn drain(&mut self, realm: Option<&str>, min_tier: Option<Tier>) -> Vec<Unread> {
        let (take, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut self.unread)
            .into_iter()
            .partition(|u| {
                realm.is_none_or(|r| u.realm == r)
                    && min_tier.is_none_or(|t| tier_rank(u.tier) >= tier_rank(t))
            });
        self.unread = keep;
        take
    }

    pub fn counts(&self) -> (usize, usize, usize) {
        let mut c = (0, 0, 0);
        for u in &self.unread {
            match u.tier {
                Tier::Immediate => c.0 += 1,
                Tier::Batched => c.1 += 1,
                Tier::Silent => c.2 += 1,
            }
        }
        c
    }

    pub fn trailer(&self) -> Option<String> {
        let (i, b, s) = self.counts();
        if i + b + s == 0 {
            return None;
        }
        Some(format!(
            "\n\n[waystation] unread: {} immediate, {} batched, {} low. Call ws_inbox to read.",
            i, b, s
        ))
    }
}

fn tier_rank(t: Tier) -> u8 {
    match t {
        Tier::Silent => 0,
        Tier::Batched => 1,
        Tier::Immediate => 2,
    }
}

pub fn classify(
    m: &Message,
    agent_id: &str,
    subscriptions: &[String],
    my_threads: &BTreeSet<String>,
) -> Tier {
    if m.kind == Kind::Escalation || m.priority == Priority::Urgent {
        return Tier::Immediate;
    }
    if m.to.iter().any(|t| t == agent_id) {
        return Tier::Immediate;
    }
    if let Some(parent) = m.reply_to
        && my_threads.contains(&parent.to_string())
    {
        return Tier::Immediate;
    }
    if m.priority == Priority::Low {
        return Tier::Silent;
    }
    if subscriptions.is_empty() || subscriptions.iter().any(|s| s == &m.channel || s == "*") {
        return Tier::Batched;
    }
    Tier::Silent
}

/// Render one unread item as a channel event: (content, meta).
pub fn render_event(u: &Unread) -> (String, BTreeMap<String, String>) {
    let m = &u.message;
    let mut meta = BTreeMap::new();
    meta.insert("id".into(), m.id.to_string());
    meta.insert("realm".into(), u.realm.clone());
    meta.insert("trust".into(), u.trust.to_string());
    meta.insert("kind".into(), m.kind.to_string());
    meta.insert("priority".into(), m.priority.to_string());
    meta.insert("from".into(), m.from.agent.clone());
    if let Some(sess) = &m.from.session {
        meta.insert("from_session".into(), sess.clone());
    }
    meta.insert("channel".into(), m.channel.clone());
    if let Some(r) = m.reply_to {
        meta.insert("reply_to".into(), r.to_string());
    }
    let mut content = String::new();
    if u.trust == Trust::External {
        content.push_str("(From an external party. Reply only with information intended for them.)\n");
    }
    content.push_str(&m.body);
    (content, meta)
}

pub fn render_digest(items: &[Unread]) -> (String, BTreeMap<String, String>) {
    let mut meta = BTreeMap::new();
    meta.insert("kind".into(), "digest".into());
    meta.insert("count".into(), items.len().to_string());
    let mut content = format!("{} new messages. Use ws_inbox for full bodies.\n", items.len());
    let mut by_realm: BTreeMap<&str, Vec<&Unread>> = BTreeMap::new();
    for u in items {
        by_realm.entry(&u.realm).or_default().push(u);
    }
    for (realm, list) in by_realm {
        let trust = list[0].trust;
        content.push_str(&format!("\n## {realm} ({trust})\n"));
        for u in list {
            content.push_str(&format!("- {}\n", u.message.summary_line()));
        }
    }
    (content, meta)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Author;

    fn msg(channel: &str) -> Message {
        Message::new(Author { agent: "x/other".into(), ..Default::default() }, channel, "hi")
    }

    #[test]
    fn classify_tiers() {
        let subs = vec!["backend".to_string()];
        let threads = BTreeSet::new();
        assert_eq!(classify(&msg("backend"), "x/me", &subs, &threads), Tier::Batched);
        assert_eq!(classify(&msg("other"), "x/me", &subs, &threads), Tier::Silent);
        let mut m = msg("other");
        m.to = vec!["x/me".into()];
        assert_eq!(classify(&m, "x/me", &subs, &threads), Tier::Immediate);
        let mut m = msg("other");
        m.priority = Priority::Urgent;
        assert_eq!(classify(&m, "x/me", &subs, &threads), Tier::Immediate);
        let mut m = msg("backend");
        m.priority = Priority::Low;
        assert_eq!(classify(&m, "x/me", &subs, &threads), Tier::Silent);
        // A sibling session of the same agent gets ordinary routing: unsubscribed → silent.
        let mut m = msg("other");
        m.from.agent = "x/me".into();
        m.from.session = Some("sibling".into());
        assert_eq!(classify(&m, "x/me", &subs, &threads), Tier::Silent);
        let mut m = msg("backend");
        m.from.agent = "x/me".into();
        assert_eq!(classify(&m, "x/me", &subs, &threads), Tier::Batched);
    }
}
