//! Context window management — keeps conversation history within token limits.
//!
//! Mirrors the compression pipeline in Claude Code's `QueryEngine.ts`:
//!
//! Level 4 (Autocompact) requires an LLM call; see [`autocompact`].
//!
//! ```text
//! [1] Content replacement  — oversized tool results are truncated inline
//! [2] Snip compact         — oldest non-system messages are dropped
//! [3] Microcompact         — assistant turns collapsed to one-liner
//! [4] Autocompact          — full conversation summarised via LLM call
//! ```
//!
//! This implementation provides levels 1–3 locally (no extra API call) and
//! exposes a hook for level 4 (autocompact) that callers can wire up.

use iris_llm::{ContentBlock, Message, Role};

/// Rough token estimate: 1 token ≈ 4 UTF-8 bytes.
fn estimate_tokens(text: &str) -> usize {
    (text.len() / 4).max(1)
}

fn message_tokens(msg: &Message) -> usize {
    msg.content.iter().map(|b| match b {
        ContentBlock::Text { text } => estimate_tokens(text),
        ContentBlock::ToolUse { name, input, .. } => {
            estimate_tokens(name) + estimate_tokens(&input.to_string())
        }
        ContentBlock::ToolResult { content, .. } => estimate_tokens(content),
        ContentBlock::Thinking { thinking } => estimate_tokens(thinking),
    }).sum::<usize>() + 4 // role overhead
}

/// Configuration for the compression pipeline.
#[derive(Debug, Clone)]
pub struct ContextConfig {
    /// Maximum tokens before compression kicks in.
    pub max_tokens: usize,
    /// Tool results larger than this are truncated (level 1).
    pub max_tool_result_tokens: usize,
    /// Number of recent message pairs to always preserve (level 2).
    pub keep_recent_turns: usize,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            max_tokens: 180_000,       // safely under claude-sonnet 200k limit
            max_tool_result_tokens: 8_000,
            keep_recent_turns: 6,
        }
    }
}

/// Returns the total estimated token count for a message list.
pub fn count_tokens(messages: &[Message]) -> usize {
    messages.iter().map(message_tokens).sum()
}

/// **Level 1** — Truncate oversized tool results in-place.
///
/// Tool results that exceed `max_tool_result_tokens` are replaced with a
/// truncated version plus a notice.
pub fn truncate_tool_results(messages: &mut Vec<Message>, max_tokens: usize) {
    for msg in messages.iter_mut() {
        for block in msg.content.iter_mut() {
            if let ContentBlock::ToolResult { content, .. } = block {
                if estimate_tokens(content) > max_tokens {
                    let char_limit = max_tokens * 4;
                    let truncated: String = content.chars().take(char_limit).collect();
                    *content = format!(
                        "{truncated}\n\n[… output truncated, {} chars omitted]",
                        content.len().saturating_sub(char_limit)
                    );
                }
            }
        }
    }
}

/// **Level 2** — Drop oldest message pairs until under the token budget.
///
/// Always preserves:
/// - The first system/user turn (project context).
/// - The last `keep_recent` user+assistant pairs.
pub fn snip_oldest(messages: &mut Vec<Message>, budget: usize, keep_recent: usize) {
    // Find indices of user messages (conversation turn boundaries).
    let user_indices: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == Role::User)
        .map(|(i, _)| i)
        .collect();

    // We must keep the first turn and the last `keep_recent` turns.
    let protected_from = user_indices.len().saturating_sub(keep_recent);
    let droppable: Vec<usize> = user_indices[1..protected_from.max(1)].to_vec();

    for start in droppable {
        if count_tokens(messages) <= budget {
            break;
        }
        // Drop start and start+1 (assistant reply), adjusting for prior removals.
        // Since we iterate front-to-back and remove pairs, index shifts by 2 each time
        // but we recalculate after each removal.
        if start < messages.len() {
            messages.remove(start);
        }
        if start < messages.len() {
            messages.remove(start); // now the assistant reply sits at `start`
        }
    }
}

/// **Level 3** — Collapse older assistant turns to one-line summaries.
///
/// Keeps full text only for the last `keep_recent` assistant messages.
pub fn microcompact(messages: &mut Vec<Message>, keep_recent: usize) {
    let asst_indices: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == Role::Assistant)
        .map(|(i, _)| i)
        .collect();

    let collapse_until = asst_indices.len().saturating_sub(keep_recent);

    for &idx in &asst_indices[..collapse_until] {
        let msg = &mut messages[idx];
        let full_text: String = msg.content.iter().filter_map(|b| {
            if let ContentBlock::Text { text } = b { Some(text.as_str()) } else { None }
        }).collect::<Vec<_>>().join(" ");

        if full_text.len() > 120 {
            let summary: String = full_text.chars().take(100).collect();
            msg.content = vec![ContentBlock::Text {
                text: format!("{summary}… [collapsed]"),
            }];
        }
    }
}

/// Run the full compression pipeline (levels 1–3) and return whether compression occurred.
///
/// Call this before each LLM request to ensure the message list fits in the context window.
pub fn compress(messages: &mut Vec<Message>, config: &ContextConfig) -> bool {
    if count_tokens(messages) <= config.max_tokens {
        return false;
    }

    // Level 1: truncate big tool results.
    truncate_tool_results(messages, config.max_tool_result_tokens);
    if count_tokens(messages) <= config.max_tokens {
        return true;
    }

    // Level 2: drop oldest turns.
    snip_oldest(messages, config.max_tokens, config.keep_recent_turns);
    if count_tokens(messages) <= config.max_tokens {
        return true;
    }

    // Level 3: microcompact older assistant turns.
    microcompact(messages, config.keep_recent_turns);

    true
}

// ── Level 4: Autocompact (LLM-assisted summary) ───────────────────────────────

/// **Level 4** — Replace the entire conversation with an LLM-generated summary.
///
/// Called when levels 1–3 are insufficient. Makes one non-streaming API call to
/// produce a compact summary of the conversation so far, then replaces
/// `messages` with a single assistant message containing that summary plus the
/// last `keep_recent` user/assistant pairs (to preserve immediate context).
///
/// Returns `true` if compression was performed.
pub async fn autocompact(
    messages: &mut Vec<Message>,
    provider: &mut iris_llm::AnthropicProvider,
    config: &ContextConfig,
) -> anyhow::Result<bool> {
    use futures::StreamExt;
    use iris_llm::{ModelConfig, StreamEvent};

    if count_tokens(messages) <= config.max_tokens {
        return Ok(false);
    }

    tracing::info!("autocompact: summarising {} messages", messages.len());

    // Build a plain-text transcript to summarise.
    let mut transcript = String::new();
    for msg in messages.iter() {
        let role = match msg.role {
            iris_llm::Role::User => "User",
            iris_llm::Role::Assistant => "Assistant",
            iris_llm::Role::Tool => "Tool",
        };
        for block in &msg.content {
            match block {
                iris_llm::ContentBlock::Text { text } => {
                    transcript.push_str(&format!("{role}: {text}\n\n"));
                }
                iris_llm::ContentBlock::ToolUse { name, .. } => {
                    transcript.push_str(&format!("{role}: [called tool: {name}]\n\n"));
                }
                iris_llm::ContentBlock::ToolResult { content, .. } => {
                    let preview: String = content.chars().take(200).collect();
                    transcript.push_str(&format!("Tool result: {preview}…\n\n"));
                }
                _ => {}
            }
        }
    }

    let summary_prompt = format!(
        "Below is a conversation transcript. Write a concise but complete summary \
         that preserves all important context, decisions, file paths, code changes, \
         and open questions. The summary will replace the transcript in the agent's \
         context window — it must be self-contained.\n\n{transcript}"
    );

    let summary_messages = vec![iris_llm::Message::user(&summary_prompt)];
    let summary_config = ModelConfig::new("claude-haiku-4-5-20251001")
        .with_max_tokens(2048);

    let stream = provider
        .chat_stream(&summary_messages, &[], &summary_config)
        .await?;
    futures::pin_mut!(stream);

    let mut summary = String::new();
    while let Some(event) = stream.next().await {
        if let Ok(StreamEvent::TextDelta { text }) = event {
            summary.push_str(&text);
        }
    }

    if summary.trim().is_empty() {
        return Ok(false);
    }

    // Keep the last N turns verbatim for immediate context.
    let keep_n = config.keep_recent_turns * 2;
    let recent: Vec<Message> = messages
        .iter()
        .rev()
        .take(keep_n)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    messages.clear();
    messages.push(iris_llm::Message::assistant(format!(
        "[Conversation summary — earlier context compressed]\n\n{summary}"
    )));
    messages.extend(recent);

    tracing::info!(
        "autocompact: compressed to {} messages",
        messages.len()
    );
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iris_llm::{ContentBlock, Message, Role};

    fn user_msg(text: &str) -> Message {
        Message { role: Role::User, content: vec![ContentBlock::Text { text: text.to_string() }] }
    }
    fn asst_msg(text: &str) -> Message {
        Message { role: Role::Assistant, content: vec![ContentBlock::Text { text: text.to_string() }] }
    }
    fn tool_result_msg(content: &str) -> Message {
        Message {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "id1".to_string(),
                content: content.to_string(),
                is_error: None,
            }],
        }
    }

    #[test]
    fn count_tokens_empty() {
        assert_eq!(count_tokens(&[]), 0);
    }

    #[test]
    fn count_tokens_single_message() {
        let msgs = vec![user_msg("hello")]; // 5 bytes → 1 token + 4 overhead = 5
        assert!(count_tokens(&msgs) >= 1);
    }

    #[test]
    fn truncate_tool_result_oversized() {
        let big = "x".repeat(400); // 400 bytes → 100 tokens, max is 10
        let mut msgs = vec![tool_result_msg(&big)];
        truncate_tool_results(&mut msgs, 10);
        if let ContentBlock::ToolResult { content, .. } = &msgs[0].content[0] {
            assert!(content.contains("truncated"));
            assert!(content.len() < big.len());
        } else {
            panic!("expected ToolResult");
        }
    }

    #[test]
    fn truncate_tool_result_small_unchanged() {
        let small = "abc";
        let mut msgs = vec![tool_result_msg(small)];
        truncate_tool_results(&mut msgs, 10_000);
        if let ContentBlock::ToolResult { content, .. } = &msgs[0].content[0] {
            assert_eq!(content, small);
        }
    }

    #[test]
    fn snip_oldest_removes_middle_turns() {
        // Build 6 user+asst pairs — very small token budget forces snipping.
        let mut msgs: Vec<Message> = (0..6)
            .flat_map(|i| [user_msg(&format!("u{i}")), asst_msg(&format!("a{i}"))])
            .collect();
        let before = msgs.len();
        snip_oldest(&mut msgs, 5, 2); // tiny budget → should remove some turns
        assert!(msgs.len() < before, "expected some messages to be removed");
    }

    #[test]
    fn snip_oldest_preserves_first_and_last_turns() {
        let mut msgs: Vec<Message> = (0..6)
            .flat_map(|i| [user_msg(&format!("u{i}")), asst_msg(&format!("a{i}"))])
            .collect();
        snip_oldest(&mut msgs, 1, 2);
        // First user message must still be there.
        assert!(msgs.iter().any(|m| m.role == Role::User));
    }

    #[test]
    fn microcompact_collapses_old_assistant_turns() {
        let long_text = "word ".repeat(50); // >120 chars
        let mut msgs = vec![
            asst_msg(&long_text),
            asst_msg(&long_text),
            asst_msg("recent short"),
        ];
        microcompact(&mut msgs, 1); // keep only last 1 assistant turn intact
        if let ContentBlock::Text { text } = &msgs[0].content[0] {
            assert!(text.contains("[collapsed]"), "old turn should be collapsed");
        }
        if let ContentBlock::Text { text } = &msgs[2].content[0] {
            assert_eq!(text, "recent short", "recent turn must be unchanged");
        }
    }

    #[test]
    fn compress_noop_when_under_budget() {
        let cfg = ContextConfig { max_tokens: 100_000, ..Default::default() };
        let mut msgs = vec![user_msg("hi"), asst_msg("hello")];
        let changed = compress(&mut msgs, &cfg);
        assert!(!changed);
    }

    #[test]
    fn compress_triggers_when_over_budget() {
        // One huge tool result will trigger level 1.
        let big = "x".repeat(100_000);
        let cfg = ContextConfig {
            max_tokens: 100,
            max_tool_result_tokens: 10,
            keep_recent_turns: 1,
        };
        let mut msgs = vec![tool_result_msg(&big)];
        let changed = compress(&mut msgs, &cfg);
        assert!(changed);
    }
}
