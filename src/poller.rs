//! Turn new commits on a realm into events.

use crate::model::{Message, Presence};
use crate::store::Store;
use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(dead_code)]
pub enum Event {
    Message { realm: String, commit: String, message: Message },
    Presence { realm: String, presence: Presence },
}

impl Event {
    #[allow(dead_code)]
    pub fn realm(&self) -> &str {
        match self {
            Event::Message { realm, .. } | Event::Presence { realm, .. } => realm,
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Tick {
    pub events: Vec<Event>,
    /// Remote head after this tick, to be persisted as the cursor.
    pub head: Option<String>,
}

/// One poll: fetch, diff `cursor..remote_head`, sync the worktree.
///
/// A `None` cursor means "first sight of this realm": no history is replayed,
/// the cursor simply starts at the current head.
pub fn tick(store: &Store, cursor: Option<&str>) -> Result<Tick> {
    store.repo.fetch()?;
    let Some(head) = store.repo.remote_head()? else {
        return Ok(Tick::default());
    };
    let tick = diff(store, cursor, &head)?;
    store.repo.sync_worktree()?;
    Ok(tick)
}

/// Events for files added between `cursor` and `head`, read from the local clone
/// without touching the network.
pub fn diff(store: &Store, cursor: Option<&str>, head: &str) -> Result<Tick> {
    let mut events = Vec::new();
    if let Some(from) = cursor
        && from != head
    {
        let added = match store.repo.added_files(Some(from), head) {
            Ok(a) => a,
            Err(e) => {
                // Unknown cursor (history rewritten, fresh clone): start over at head.
                tracing::warn!(realm = %store.realm, "cursor {from} unusable ({e:#}); resetting to head");
                Vec::new()
            }
        };
        for path in added {
            if Message::is_message_path(&path) {
                match store.repo.read_at(head, &path).and_then(|t| Message::from_markdown(&t)) {
                    Ok(message) => events.push(Event::Message {
                        realm: store.realm.clone(),
                        commit: head.to_string(),
                        message,
                    }),
                    Err(e) => tracing::warn!(%path, "unparsable message: {e:#}"),
                }
            } else if Presence::is_presence_path(&path)
                && let Ok(presence) =
                    store.repo.read_at(head, &path).and_then(|t| Presence::from_markdown(&t))
            {
                events.push(Event::Presence { realm: store.realm.clone(), presence });
            }
        }
    }
    Ok(Tick { events, head: Some(head.to_string()) })
}
