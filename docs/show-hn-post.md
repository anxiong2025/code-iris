# Show HN Post — Ready to paste

---

## Title

```
Show HN: Code Iris – Rust AI coding agent with typed multi-agent pipeline (open source)
```

---

## Body

I've been building Code Iris, an open-source AI coding agent written entirely in Rust. Single binary, ~15MB, starts in ~1ms.

**The interesting part:** a typed serial pipeline for multi-agent tasks.

Most AI coding tools either run a single agent loop or use ad-hoc subagent spawning. Code Iris has a structured handoff protocol: each pipeline step gets the previous step's output injected into its system context as structured markdown — not concatenated into the user message.

```
iris plan "add rate limiting to the API"

→ explorer agent   (read-only, haiku)   analyzes codebase
      ↓ output injected as system context
→ reviewer agent   (read-only)          designs architecture
      ↓ both outputs injected as system context
→ worker agent     (full permissions)   writes the code
```

The types look like:
```rust
pub struct PipelineStep {
    pub label: String,
    pub agent_type: Option<String>,  // "explorer" | "worker" | "reviewer"
    pub prompt: String,
}
```

After each step completes, results accumulate as:
```
# Prior step results
## explorer
<structured analysis>
## reviewer
<architecture plan>
```

This means the worker sees everything the previous agents produced, structured and accessible.

**Other things I built:**

- 14 tools including a native LSP client (hover/go-to-definition/diagnostics via JSON-RPC stdio)
- Persistent bash session — `cd` and `export` persist across tool calls
- Hooks system (PreToolUse/PostToolUse/Notification) — `.iris/hooks.toml`
- Layered instructions — `.iris/instructions.md` prepended to system prompt, like CLAUDE.md
- 17+ LLM providers — Anthropic, OpenAI, Gemini, DeepSeek, Qwen, etc. — auto-detected from env vars
- Context autocompact at 80% of window (LLM-summarised)
- `iris doc-sync --since HEAD~1` — detects stale documentation sections after commits
- Ratatui TUI with syntax highlighting (syntect), cursor editing, command history

**Why Rust?** Mostly because I wanted to. But it means: single binary distribution, no npm/pip, no Node.js runtime, ~15MB memory vs 100MB+ for Electron-based tools.

**What's missing:** no GUI, no cloud sync, no team features. It's a CLI/TUI tool.

GitHub: https://github.com/anxiong2025/code-iris

Happy to answer questions about the pipeline protocol design, the LSP integration, or anything else.

---

## 中文版（掘金 / V2EX）标题

```
我用 Rust 写了一个 AI 编程 Agent，开源，单二进制，支持 17+ 大模型
```

## 中文版副标题关键点

- 核心差异：串行 pipeline，每步结果结构化注入下一步
- 对标 Claude Code 但开源、多提供商
- iris-core 可作为 SDK 构建自己的 agent
- 持久 bash session、LSP 工具、Hooks、分层 Instructions
- 90 个测试，Rust 实现，零 npm 依赖
