//! SendMessageTool — inter-agent messaging via a shared in-process bus.
//!
//! Allows one agent to post a message to a named "inbox" that another agent
//! (or the coordinator) can read. Mirrors Claude Code's `SendMessage` tool
//! used for agent-to-agent coordination.
//!
//! The bus is a `tokio::sync::broadcast` channel wrapped in an `Arc`. Agents
//! share the same `MessageBus` instance (injected at registry build time).
//! Messages are JSON-serialisable and carry a `from` / `to` routing tag.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::broadcast;

use super::Tool;

/// Capacity of the broadcast channel (messages are dropped if not consumed fast enough).
const BUS_CAPACITY: usize = 256;

/// A single message on the bus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusMessage {
    /// Sender identity (e.g. `"agent-0"`, `"coordinator"`).
    pub from: String,
    /// Intended recipient, or `"*"` for broadcast.
    pub to: String,
    /// Arbitrary payload.
    pub content: String,
}

/// Shared in-process message bus.
///
/// Clone freely — all clones share the same underlying channel.
#[derive(Clone)]
pub struct MessageBus {
    tx: Arc<broadcast::Sender<BusMessage>>,
}

impl Default for MessageBus {
    fn default() -> Self {
        Self::new()
    }
}

impl MessageBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(BUS_CAPACITY);
        Self { tx: Arc::new(tx) }
    }

    /// Send a message onto the bus.
    pub fn send(&self, msg: BusMessage) -> Result<()> {
        // It's fine if there are no receivers yet.
        let _ = self.tx.send(msg);
        Ok(())
    }

    /// Subscribe to receive future messages.
    pub fn subscribe(&self) -> broadcast::Receiver<BusMessage> {
        self.tx.subscribe()
    }
}

// ── Tool implementation ───────────────────────────────────────────────────────

pub struct SendMessageTool {
    pub bus: MessageBus,
    pub agent_id: String,
}

#[async_trait]
impl Tool for SendMessageTool {
    fn name(&self) -> &str {
        "send_message"
    }

    fn description(&self) -> &str {
        "Send a message to another agent or the coordinator. \
         Use this to delegate sub-tasks, report results, or request information \
         from a peer agent in a multi-agent workflow."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "to": {
                    "type": "string",
                    "description": "Recipient agent ID, or \"*\" to broadcast to all agents."
                },
                "content": {
                    "type": "string",
                    "description": "The message payload."
                }
            },
            "required": ["to", "content"]
        })
    }

    async fn execute(&self, input: Value) -> Result<String> {
        let to = input
            .get("to")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required field: to"))?
            .to_string();
        let content = input
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required field: content"))?
            .to_string();

        let msg = BusMessage {
            from: self.agent_id.clone(),
            to: to.clone(),
            content,
        };
        self.bus.send(msg)?;
        Ok(format!("Message sent to `{to}`."))
    }
}
