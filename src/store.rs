//! Realm-level read/write on top of `Repo`.

use crate::config::Trust;
use crate::model::{Message, Presence};
use crate::repo::Repo;
use anyhow::{Context, Result};
use ulid::Ulid;

#[derive(Debug, Clone)]
pub struct Store {
    pub realm: String,
    pub trust: Trust,
    pub agent_id: String,
    pub subscriptions: Vec<String>,
    pub repo: Repo,
}

impl Store {
    pub fn post(&self, msg: &Message) -> Result<String> {
        let text = msg.to_markdown()?;
        let commit = format!("post({}): {} {} {}", msg.channel, msg.kind, msg.from.agent, msg.id);
        self.repo.commit_and_push(&[(msg.path(), text)], &commit)
    }

    pub fn write_presence(&self, p: &Presence) -> Result<String> {
        let text = p.to_markdown()?;
        let commit = format!(
            "presence: {}{} {}",
            p.agent,
            p.session.as_deref().map(|s| format!("@{s}")).unwrap_or_default(),
            p.status
        );
        self.repo.commit_and_push(&[(p.path(), text)], &commit)
    }

    pub fn list_messages(&self, channel: Option<&str>, limit: usize) -> Result<Vec<Message>> {
        let dir = match channel {
            Some(c) => format!("channels/{c}"),
            None => "channels".to_string(),
        };
        let mut msgs = Vec::new();
        for path in self.repo.list_worktree(&dir)? {
            if !Message::is_message_path(&path) {
                continue;
            }
            match self.repo.read_worktree(&path).and_then(|t| Message::from_markdown(&t)) {
                Ok(m) => msgs.push(m),
                Err(e) => tracing::warn!(%path, "skipping unparsable message: {e:#}"),
            }
        }
        msgs.sort_by_key(|m| std::cmp::Reverse(m.id));
        msgs.truncate(limit);
        Ok(msgs)
    }

    pub fn find_message(&self, id: Ulid) -> Result<Option<Message>> {
        let needle = format!("/{id}.md");
        for path in self.repo.list_worktree("channels")? {
            if path.ends_with(&needle) {
                let text = self.repo.read_worktree(&path)?;
                return Ok(Some(Message::from_markdown(&text)?));
            }
        }
        Ok(None)
    }

    pub fn list_agents(&self) -> Result<Vec<Presence>> {
        let mut out = Vec::new();
        for path in self.repo.list_worktree("agents")? {
            if !Presence::is_presence_path(&path) {
                continue;
            }
            match self.repo.read_worktree(&path).and_then(|t| Presence::from_markdown(&t)) {
                Ok(p) => out.push(p),
                Err(e) => tracing::warn!(%path, "skipping unparsable presence: {e:#}"),
            }
        }
        out.sort_by(|a, b| a.agent.cmp(&b.agent));
        Ok(out)
    }

    #[allow(dead_code)]
    pub fn list_channels(&self) -> Result<Vec<String>> {
        let root = self.repo.path.join("channels");
        let mut out = Vec::new();
        if root.exists() {
            for e in std::fs::read_dir(&root).context("listing channels")? {
                let e = e?;
                if e.path().is_dir() {
                    out.push(e.file_name().to_string_lossy().to_string());
                }
            }
        }
        out.sort();
        Ok(out)
    }
}
