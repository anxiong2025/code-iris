# Pipeline Handoff Protocol in Code Iris

## The Problem: Passing Work Between Agents

Multi-agent systems face a fundamental data handoff question: when one agent finishes its work, how does the next agent receive it in a way that is structured, type-safe, and non-bloating?

Two naive approaches fail in practice.

**Shared filesystem convention** (used by systems like gstack) has agents write results to agreed-upon paths. The downstream agent knows to look at, say, `./tmp/product-analysis.txt`. This is fragile: every agent must know every other agent's naming conventions, there are race conditions if agents run concurrently, and there is no way to type-check or validate the handoff payload at compile time. Renaming a label silently breaks all consumers.

**Raw message concatenation** — just prepending the previous agent's full output into the next agent's user message — creates context bloat. Every downstream agent gets increasingly long user turns with no structural signal about what each block of text means. The third agent in a chain receives a wall of text with no reliable way to distinguish "product analysis" from "architecture plan" from its own task prompt. Parsing that structure out is brittle.

## Code Iris's Solution: Structured Context Injection

Code Iris solves this with a primitive called **structured context injection**, implemented in `pipeline_run()` in `crates/iris-core/src/coordinator.rs`.

The key types are:

```rust
pub struct PipelineStep {
    pub label: String,               // e.g. "product", "architecture"
    pub agent_type: Option<String>,  // e.g. Some("explorer"), Some("worker")
    pub system_prompt: String,
    pub prompt: String,
}

pub struct PipelineStepResult {
    pub label: String,
    pub text: String,
    pub usage: iris_llm::TokenUsage,
}
```

After each step completes, `pipeline_run()` builds a structured context block from all prior results and injects it into the *system prompt* of the next agent — not the user message:

```rust
let mut ctx = String::from("# Prior step results\n\n");
for prev in &results {
    ctx.push_str(&format!("## {}\n\n{}\n\n---\n\n", prev.label, prev.text));
}
```

This produces a system prompt section that looks like:

```
# Prior step results

## product

<full product analysis text>

---

## architecture

<full architecture plan text>

---
```

The downstream agent's *user message* stays clean — it is just the task prompt for the current step. All accumulated context lives in the system channel, where it is semantically separate from the conversational turn.

### Why This Is the Right Primitive

There are three properties that make this approach work well:

**1. Full structured access by section heading.** Each prior step's output has a markdown `##` heading with its label. Any step that needs to reference prior work can simply say "based on the product analysis above" and the model has a clear structural anchor to refer back to. There is no ambiguity about where one step's output ends and another begins.

**2. No user-turn bloat.** The prior context is injected into the system prompt, not appended to the user message. This keeps the user-turn prompt semantically clean and prevents the model from treating accumulated prior results as part of the current instruction. The model reads the system prompt as background context and the user prompt as its actual task.

**3. Type safety at the Rust level.** Each step's result is a `PipelineStepResult` — a concrete struct with a typed `label`, `text`, and `usage` field. The coordinator owns the accumulation logic in one place. There is no convention to agree on, no file path to coordinate, and no untyped blob passing between agents. If you rename a step label, the change is localized to the coordinator; the rendered markdown heading updates automatically.

## The `iris plan` Three-Step Pipeline

The `cmd_plan()` function in `crates/iris-cli/src/main.rs` demonstrates the canonical use of this protocol. Running `iris plan "add user auth"` executes:

```
Step 1  label="product"         agent_type="explorer"   (read-only, haiku model)
        prompt: "Analyse requirement: problem statement, users, criteria, constraints"
        → PipelineStepResult { label: "product", text: "..." }

Step 2  label="architecture"    agent_type="reviewer"   (read-only)
        system: "# Prior step results\n## product\n<step 1 output>"
        prompt: "Based on the product analysis above, produce a technical architecture plan"
        → PipelineStepResult { label: "architecture", text: "..." }

Step 3  label="implementation"  agent_type="worker"     (full permissions)
        system: "# Prior step results\n## product\n...\n## architecture\n..."
        prompt: "Based on the product analysis and architecture above, generate the implementation"
        → PipelineStepResult { label: "implementation", text: "..." }
```

Each step is informed by everything before it. The architecture step has read the product analysis. The implementation step has read both. No step has to re-derive context that a prior step already established.

## Permission Cascade

Each `PipelineStep` can specify an `agent_type` which resolves to an `AgentDefinition`:

```rust
pub struct AgentDefinition {
    pub name: String,
    pub sandbox_mode: SandboxMode,  // ReadOnly | Full
    pub model: Option<String>,
    pub instructions: String,
    // ...
}
```

The three built-in agent types map to:

| Agent type | Sandbox mode | Permission mode | Model |
|---|---|---|---|
| `explorer` | `ReadOnly` | `Plan` | claude-haiku (fast) |
| `reviewer` | `ReadOnly` | `Plan` | inherits from coordinator |
| `worker` | `Full` | `Auto` | inherits from coordinator |

The `Coordinator` enforces a **permission ceiling**: a step can only have *less* permission than its parent coordinator, never more. The logic is:

```rust
fn most_restrictive(a: PermissionMode, b: PermissionMode) -> PermissionMode {
    fn rank(m: &PermissionMode) -> u8 {
        match m {
            PermissionMode::Plan => 0,    // most restrictive
            PermissionMode::Default => 1,
            PermissionMode::Auto => 2,    // least restrictive
            PermissionMode::Custom { .. } => 1,
        }
    }
    if rank(&a) <= rank(&b) { a } else { b }
}
```

This means if you spawn a coordinator in `Plan` mode (read-only), even a `worker` step — which nominally has `Full` (`Auto`) permissions — will run in `Plan` mode. Privilege can only flow downward.

In the `iris plan` pipeline this shows up deliberately: the `explorer` product step and the `reviewer` architecture step are read-only and use a fast/cheap model. Only the `worker` implementation step has write permissions, and it runs only after the cheaper steps have already done the analysis and design work.

## Parallel vs Serial: Choosing the Right Primitive

The coordinator offers two execution modes:

**`run()` — parallel fan-out.** All sub-tasks run concurrently as separate Tokio tasks. Results come back in submission order. Useful when tasks are independent: "search the auth module", "search the database layer", "check the test coverage" can all run simultaneously. Results are collected and passed to an optional synthesis agent.

**`pipeline_run()` — serial with context injection.** Each step waits for the previous step to complete before it runs, and receives all prior outputs in its system prompt. Suitable when steps have logical dependencies: you cannot write the architecture without first understanding the requirements, and you cannot write the code without first having the architecture.

Both modes respect `CoordinatorConfig`:

```rust
pub struct CoordinatorConfig {
    pub max_threads: usize,  // default: 6
    pub max_depth: u8,       // default: 1
}
```

`max_depth: 1` means sub-agents cannot themselves spawn sub-agents. This prevents exponential fan-out: a coordinator at depth 0 can spawn sub-agents, but those sub-agents cannot themselves act as coordinators. The check fires immediately:

```rust
if self.depth >= self.config.max_depth {
    bail!("coordinator depth limit reached (max_depth={})", self.config.max_depth);
}
```

## MessageBus: Peer-to-Peer Alongside the Pipeline

For cases where agents need to communicate outside the pipeline handoff pattern — delegating a sub-task, reporting interim findings, requesting clarification from a peer — the `MessageBus` provides a separate channel:

```rust
pub struct BusMessage {
    pub from: String,   // sender agent ID, e.g. "pipe-0"
    pub to: String,     // recipient ID, or "*" for broadcast
    pub content: String,
}
```

The bus is a `tokio::sync::broadcast` channel wrapped in an `Arc`, so all agents in a coordinator run share the same underlying channel. Each agent gets a `SendMessageTool` in its registry, wired to the shared bus. Agents can call it like any other tool: `send_message(to="pipe-2", content="...")`.

The `MessageBus` is complementary to structured context injection, not a replacement for it. Context injection is for structured, ordered handoffs between pipeline steps. The bus is for unordered, peer-to-peer coordination that cannot be anticipated in advance.

## CLI Syntax

The `iris run --pipeline` flag exposes `pipeline_run()` directly from the command line:

```
iris run --pipeline \
  --sub "product@explorer:Analyse the auth requirement" \
  --sub "arch@reviewer:Design the technical architecture" \
  --sub "impl@worker:Write the implementation with tests"
```

The step format is `label@agent_type:prompt`. The `@agent_type` part is optional; omitting it uses the coordinator's default model with no special agent definition.

The `iris plan` subcommand is a convenience wrapper around this pattern with pre-built prompts tuned for product analysis, architecture design, and implementation generation:

```
iris plan "add JWT authentication to the REST API"
iris plan "add JWT authentication to the REST API" --arch-only
```

## Summary

The handoff protocol's core insight is that **structured markdown injection into the system prompt** is a better primitive than either shared state or concatenated user messages. It gives each downstream agent typed, labeled, structured access to all prior work without polluting the user turn, without requiring file system coordination, and without losing the Rust type system's enforcement of the handoff interface. The permission cascade ensures that cheap read-only agents do the analysis and design work first, and only the privileged worker agent makes changes — and only once it has the full context it needs.
