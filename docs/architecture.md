# Code Iris — Architecture

> Version: 0.1.x · Last updated: 2026-04

## Overview

Code Iris is a Rust-native AI coding agent that combines a streaming LLM loop, a multi-agent orchestration layer, a 14-tool execution environment, and first-class support for three LLM provider families. It ships two entry points: a `ratatui`-based terminal UI (`code-iris`) and a headless CLI (`iris`).

```
┌────────────────────────────────────────────────────────┐
│                     User Interface                     │
│   iris-tui (ratatui TUI)   │   iris-cli (clap CLI)     │
└────────────────────────────────────────────────────────┘
                       │               │
                       ▼               ▼
┌────────────────────────────────────────────────────────┐
│                     iris-core                          │
│  ┌─────────────┐  ┌─────────────┐  ┌───────────────┐  │
│  │    Agent    │  │ Coordinator │  │  ToolRegistry  │  │
│  │  (chat loop)│  │(multi-agent)│  │  (14 tools)   │  │
│  └─────────────┘  └─────────────┘  └───────────────┘  │
└────────────────────────────────────────────────────────┘
                             │
                             ▼
┌────────────────────────────────────────────────────────┐
│                     iris-llm                           │
│  Anthropic │ OpenAI-compatible │ Google Gemini         │
└────────────────────────────────────────────────────────┘
```

## Workspace Layout

| Crate | Role |
|-------|------|
| `iris-core` | Agent logic, tool execution, multi-agent orchestration, permission model |
| `iris-llm` | Provider abstraction, SSE streaming, OAuth and API-key auth |
| `iris-tui` | Interactive terminal UI built on `ratatui` |
| `iris-cli` | Headless CLI built on `clap`; defines `plan`, `doc-sync`, `run` commands |

## Agent Loop

`Agent::chat_streaming()` drives the core interaction cycle.

```
User message
     │
     ▼
┌──────────────────────────────────────┐
│  Round 1..=20                        │
│                                      │
│  1. Build messages (system + history)│
│  2. Stream LLM response via SSE      │◄── tokio::select! (cancel flag)
│  3. Collect tool calls               │
│  4. Execute tools in ToolRegistry    │
│  5. Append results, continue loop    │
│                                      │
│  [no tool calls] → return to user    │
└──────────────────────────────────────┘
```

Key design points:

- **20-round cap** prevents runaway loops while allowing complex multi-step tasks.
- **Cancellation** is implemented via `Arc<AtomicBool>` checked inside `tokio::select!`, allowing mid-stream interruption without tearing down the async runtime.
- **4-level context compression** uses LLM-assisted summarisation (autocompact) when the context window approaches its limit. Earlier exchanges are compressed into a structured summary that preserves task state without retaining every token.

## Multi-Agent: Coordinator

When a task exceeds the capacity of a single agent, `Coordinator` manages a hierarchy of sub-agents.

```
Coordinator
├─ ToolRegistry (shared)
├─ CoordinatorConfig { max_threads: 6, max_depth: 1 }
│
├─ run_subtasks(Vec<SubTask>)          ← parallel execution
│    Spawn ≤6 agents concurrently
│    Collect outputs → synthesise
│
└─ pipeline_run(Vec<PipelineStep>)     ← serial execution
     Step N output → structured context → Step N+1
```

### Parallel sub-agents

`run_subtasks` fans out a list of `SubTask` objects across up to `max_threads` concurrent agents. Results are gathered and synthesised before returning to the parent.

### Serial pipeline

`pipeline_run` executes steps sequentially. Each step's output is injected into the next step as a structured system context block:

```
# Prior step results
## product
<text>
## architecture
<text>
```

This lets later steps reason about earlier decisions without the entire conversation history being replayed.

### Permission ceiling

`most_restrictive(a, b) -> PermissionMode` ensures a sub-agent can never operate with broader permissions than its parent. The hierarchy is: `ReadOnly < Default < Plan < Auto < Custom(Full)`. A `depth: u8` guard bails when `max_depth` is reached, preventing unbounded recursive delegation.

## Agent Type System

Agents are described by `AgentDefinition { name, description, instructions, model, sandbox_mode }`.

`SandboxMode` has two variants:

| Variant | Capabilities |
|---------|-------------|
| `ReadOnly` | Read files, grep, glob, LSP hover/diagnostics, web fetch |
| `Full` | All of the above plus `bash`, `file_write`, `file_edit`, `task_*` |

Three built-in agents are always available:

| Name | Mode | Model |
|------|------|-------|
| `explorer` | ReadOnly | Claude Haiku (fast, cheap) |
| `worker` | Full | Configured default |
| `reviewer` | ReadOnly | Configured default |

Custom agents are loaded from `.iris/agents/*.toml` (project-local) and `~/.code-iris/agents/*.toml` (user-global). `find_agent(name, project_root)` resolves custom definitions first, falling back to built-ins. This lets teams define specialised agents (e.g., a `db-migration` agent with specific instructions) without forking the binary.

## Tool System

The 14 tools in `ToolRegistry` are grouped by concern:

| Group | Tools |
|-------|-------|
| I/O | `bash`, `file_read`, `file_write`, `file_edit`, `grep`, `glob` |
| Intelligence | `lsp` |
| Web | `web_fetch`, `web_search` |
| Orchestration | `task_create`, `task_update`, `task_list`, `task_get`, `agent_tool`, `send_message` |

All I/O tools share a `CwdRef = Arc<Mutex<Option<PathBuf>>>` that tracks the agent's current working directory. This makes relative paths safe across async tool invocations.

The `lsp` tool communicates with a language server over JSON-RPC on stdio, exposing hover, go-to-definition, find-references, and diagnostics. This gives the agent semantic understanding of the codebase beyond text matching.

`send_message` publishes to a `MessageBus`, allowing sibling agents spawned by the same `Coordinator` to exchange structured data without going through the LLM.

## LLM Provider Layer (`iris-llm`)

```
                  ┌───────────────────┐
                  │   LlmClient trait │
                  └─────────┬─────────┘
           ┌────────────────┼────────────────┐
           ▼                ▼                ▼
    AnthropicClient   OpenAIClient    GeminiClient
    (SSE, OAuth+key)  (17+ providers) (SSE, delta diff)
```

Auto-detection order: OAuth token → `ANTHROPIC_API_KEY` → other environment keys. This means a developer with Claude OAuth set up gets a seamless zero-configuration experience, while CI environments can override via env vars.

Google Gemini responses use delta diffing to reconstruct streamed text correctly, working around the provider's non-standard SSE format.

## Permission Model

`PermissionMode` variants: `Default`, `Plan`, `Auto`, `Custom`.

`most_restrictive()` is the single choke point for permission propagation in the agent hierarchy — it is called every time a `Coordinator` spawns a child, so no code path can accidentally elevate permissions.

## CLI Commands (`iris-cli`)

| Command | Description |
|---------|-------------|
| `iris plan "prompt"` | 3-step serial pipeline: product spec → architecture → implementation. `--arch-only` skips the implementation step. |
| `iris doc-sync --since <ref>` | Runs `git diff` since the given ref, detects which `.md` sections are stale, and proposes updates. |
| `iris run --pipeline --sub "label@type:prompt"` | Runs an ad-hoc pipeline or sub-agent invocation. |

TUI slash commands (`/plan`, `/agents`, `/commit`, `/memory`, `/cd`, `/worktree`, …) expose the same orchestration primitives interactively.

## Key Design Decisions

**Why Rust?** Async Rust (`tokio`) makes it straightforward to implement the `tokio::select!`-based cancellation model and the concurrent sub-agent fan-out without callback hell. The borrow checker enforces the `Arc<Mutex<CwdRef>>` sharing discipline at compile time.

**Why a depth cap instead of a cycle detector?** Acyclic delegation is simpler to reason about and audit. `max_depth: 1` is the right default for the current use cases; the config is exposed for advanced users who understand the trade-offs.

**Why serial pipeline over parallel for `iris plan`?** The architecture step needs the product spec as input; the implementation step needs the architecture. The dependency is linear, so a serial pipeline with injected context is both correct and easy to debug.

**Why LLM-assisted autocompact instead of simple truncation?** Simple truncation loses task state. Summarisation preserves intent, constraints, and intermediate decisions, which are exactly what a long-running agent needs to stay on track.
