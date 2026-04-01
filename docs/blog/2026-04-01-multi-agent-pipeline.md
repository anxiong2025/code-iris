# Show HN: I built a Rust AI coding agent with a typed multi-agent pipeline

**GitHub:** https://github.com/anxiong2025/code-iris

---

Most AI coding tools are wrappers — they glue an LLM to a shell and call it an agent. I wanted to understand what it actually takes to build one from scratch, and I had a specific architectural itch to scratch: what does a *correct* multi-agent handoff look like when agents need to share structured context, not just chat history?

The result is **code-iris**, a terminal-based AI coding agent written in Rust. It ships with 14 tools (bash, file read/write/edit, grep, glob, LSP integration, web fetch/search, task management, sub-agents), a ratatui TUI, multi-provider LLM support, and a typed multi-agent pipeline at its core. This post is about that pipeline.

---

## The design problem with multi-agent handoffs

When you chain multiple agents together, each one needs the output of the previous step to do its job. The naive approach is to dump everything into a long conversation thread and let the next agent read it. That works until it doesn't: context windows fill up, agents get confused by prior tool calls intermixed with conclusions, and you have no structured way to extract "what did the planner actually decide?"

The other common pattern is file conventions — agent 1 writes `plan.md`, agent 2 reads it. This is fragile (no schema), slow (disk I/O for every step), and hard to compose programmatically.

The insight I landed on: **inject structured step results directly into the next agent's system prompt**, not its conversation history. Each completed step becomes a clearly-labeled block that the next agent sees as context, not as a prior turn it needs to respond to. The agent knows what upstream work was done without having to re-read a conversation.

---

## The pipeline handoff protocol

The core type is simple:

```rust
pub struct PipelineStep {
    pub label: String,
    pub agent_type: Option<String>,  // "explorer" | "worker" | "reviewer" | custom
    pub system_prompt: String,
    pub prompt: String,
}
```

`label` is the key. When step N finishes, its output is stored under that label. The next step's system prompt gets augmented with:

```
# Prior step results

## product
<structured output from step 1>

## architecture
<structured output from step 2>
```

`pipeline_run()` threads this through automatically — each step sees all prior results injected into its system context before it runs. The agent types control capability scoping:

- `explorer` — read-only tool access, runs on Haiku for speed, good for codebase analysis
- `reviewer` — read-only, runs on the main model, good for design and critique
- `worker` — full tool access, executes writes and shell commands

The worker at the end of the chain has everything it needs: it knows what the explorer found in the codebase and what the reviewer decided about architecture, and it can just execute.

---

## What `iris plan` actually does

```
$ iris plan "add user auth with JWT"
```

This kicks off a three-step pipeline:

1. **explorer** scans the codebase — finds existing auth-adjacent code, checks dependencies, notes the project structure. Fast, no writes.
2. **reviewer** reads the explorer's findings and designs the architecture — which files to touch, what the JWT flow looks like, what edge cases to handle. The explorer's full output is in its system context.
3. **worker** implements the plan — writes the code, edits files, runs tests if they exist. Both the explorer analysis and the reviewer design are in its system context as structured blocks.

Each step's output is streamed to the TUI in real time. You can watch the reasoning happen. At the end you have working code and a complete audit trail of why each decision was made.

The pipeline is not magic — it's a loop over `Vec<PipelineStep>` with context accumulation. But the typing makes it composable: you can define custom pipelines with arbitrary step counts and agent types, or use the three built-in archetypes.

---

## Other things worth knowing

**LSP tool.** Most coding agents treat the codebase as plain text. code-iris has a `lsp` tool that speaks JSON-RPC stdio to a running language server — hover, go-to-definition, find-references, diagnostics. The agent can ask "what type does this return?" and get an accurate answer instead of guessing from string matching. This matters a lot for refactoring tasks.

**doc-sync.** `iris doc-sync --since HEAD~1` diffs the last commit and checks whether any `.md` sections reference code that changed. If a function was renamed and the docs still use the old name, it flags it. Keeping documentation current is one of those tasks that's tedious for humans and easy for a model with diff access.

**14 tools.** bash, file_read, file_write, file_edit, grep, glob, lsp, web_fetch, web_search, and four task-management tools (create, update, list, complete) plus agent_tool and send_message for spawning sub-agents.

**Multi-provider.** Anthropic Claude, any OpenAI-compatible endpoint (17+ providers including OpenRouter, Together, Groq), and Google Gemini. Provider is auto-detected from env vars — set `ANTHROPIC_API_KEY` and it uses Anthropic; set `OPENAI_API_KEY` and it uses OpenAI. No config file required.

---

## Honest reflection

What's missing: no streaming tool results display yet (you see the tool call, wait, then see output), no persistent agent memory across sessions, no eval harness, and the LSP integration requires the user to have a language server installed and running. The pipeline definition is currently code-level — there's no YAML or DSL for defining pipelines without recompiling.

What's next: persistent task state, a pipeline definition format that doesn't require Rust, better error recovery when a step's output is malformed, and eval benchmarks to track regression as the agent improves.

The core loop — typed steps, structured context injection, scoped capabilities — feels right. The rest is iteration.

---

Source: https://github.com/anxiong2025/code-iris
