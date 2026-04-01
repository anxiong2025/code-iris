# Code Iris 的 Pipeline 移交协议设计

## 问题：多 Agent 之间如何传递工作成果

多 Agent 系统面临一个核心的数据移交问题：当一个 Agent 完成工作后，下一个 Agent 如何以结构化、类型安全、不膨胀的方式接收这份成果？

两种朴素的做法在实践中都行不通。

**共享文件系统约定**（gstack 等系统的做法）：各 Agent 将结果写到约定好的路径，比如 `./tmp/product-analysis.txt`，下游 Agent 去读这个文件。这很脆弱——每个 Agent 都必须了解其他所有 Agent 的命名约定；如果并发运行会出现竞态条件；也无法在编译期类型检查移交的数据。改个 label 名字，所有消费方就悄悄失效了。

**原始消息拼接**：把上一个 Agent 的完整输出直接拼到下一个 Agent 的用户消息里。这会导致上下文膨胀。链路中越靠后的 Agent，用户消息就越长，而且里面没有任何结构信号来区分"产品分析"和"架构方案"各自在哪里结束、在哪里开始。第三个 Agent 收到的是一堵文字墙，想可靠地解析出结构，非常脆弱。

## Code Iris 的解法：结构化上下文注入

Code Iris 用一个叫**结构化上下文注入**的原语解决了这个问题。它在 `crates/iris-core/src/coordinator.rs` 的 `pipeline_run()` 函数中实现。

核心数据类型如下：

```rust
pub struct PipelineStep {
    pub label: String,               // 例如 "product"、"architecture"
    pub agent_type: Option<String>,  // 例如 Some("explorer")、Some("worker")
    pub system_prompt: String,
    pub prompt: String,
}

pub struct PipelineStepResult {
    pub label: String,
    pub text: String,
    pub usage: iris_llm::TokenUsage,
}
```

每一步执行完毕后，`pipeline_run()` 把所有已完成步骤的结果整理成一个结构化的 Markdown 块，然后注入到**下一个 Agent 的 system prompt** 里——而不是用户消息里：

```rust
let mut ctx = String::from("# Prior step results\n\n");
for prev in &results {
    ctx.push_str(&format!("## {}\n\n{}\n\n---\n\n", prev.label, prev.text));
}
```

注入后，下游 Agent 的 system prompt 里会有这样一段：

```
# Prior step results

## product

<产品分析的完整内容>

---

## architecture

<架构方案的完整内容>

---
```

下游 Agent 的**用户消息**保持干净——只有当前步骤的任务提示词。所有积累的上下文都在 system 通道里，与对话轮次语义隔离。

### 为什么这是正确的原语

这个方案有三个关键特性：

**1. 通过 section 标题进行结构化访问。** 每个已完成步骤的输出都有一个带 label 的 `##` 标题。任何需要引用先前成果的步骤，都可以直接说"根据上面的产品分析……"，模型有清晰的结构锚点。步骤之间的输出不会混在一起，没有歧义。

**2. 用户消息不会膨胀。** 先前的上下文注入在 system prompt 里，不追加到用户消息里。这让用户消息在语义上保持干净，也防止模型把积累的先前结果当作当前指令的一部分来处理。模型读 system prompt 是背景知识，读用户 prompt 才是当前任务。

**3. Rust 层面的类型安全。** 每一步的结果是具体的 `PipelineStepResult` 结构体，有类型化的 `label`、`text`、`usage` 字段。积累逻辑统一在 Coordinator 里，没有需要各方约定的文件路径，没有无类型的 blob 在 Agent 之间传递。如果你改了某个步骤的 label，只需改一处，渲染出来的 Markdown 标题会自动跟着更新。

## `iris plan` 三步流水线

`crates/iris-cli/src/main.rs` 里的 `cmd_plan()` 展示了这个协议的典型用法。执行 `iris plan "add user auth"` 会运行：

```
Step 1  label="product"         agent_type="explorer"   (只读，haiku 模型)
        prompt: "分析需求：问题陈述、目标用户、验收标准、约束与风险"
        → PipelineStepResult { label: "product", text: "..." }

Step 2  label="architecture"    agent_type="reviewer"   (只读)
        system: "# Prior step results\n## product\n<step 1 的输出>"
        prompt: "根据上面的产品分析，输出技术架构方案"
        → PipelineStepResult { label: "architecture", text: "..." }

Step 3  label="implementation"  agent_type="worker"     (完整权限)
        system: "# Prior step results\n## product\n...\n## architecture\n..."
        prompt: "根据上面的产品分析和架构方案，生成具体实现"
        → PipelineStepResult { label: "implementation", text: "..." }
```

每一步都能看到它之前所有步骤的成果。架构步骤读过了产品分析；实现步骤读过了产品分析和架构方案两者。没有任何步骤需要重新推导已有步骤已经得出的结论。

## 权限级联

每个 `PipelineStep` 可以指定一个 `agent_type`，它会被解析成对应的 `AgentDefinition`：

```rust
pub struct AgentDefinition {
    pub name: String,
    pub sandbox_mode: SandboxMode,  // ReadOnly | Full
    pub model: Option<String>,
    pub instructions: String,
    // ...
}
```

三种内置 Agent 类型的权限映射如下：

| Agent 类型 | 沙箱模式 | 权限模式 | 模型 |
|---|---|---|---|
| `explorer` | `ReadOnly` | `Plan`（只读） | claude-haiku（快速） |
| `reviewer` | `ReadOnly` | `Plan`（只读） | 继承自 Coordinator |
| `worker` | `Full` | `Auto`（完整权限） | 继承自 Coordinator |

Coordinator 会强制执行**权限上限**：一个步骤的权限只能比其父 Coordinator 更受限，不能更宽松。具体逻辑：

```rust
fn most_restrictive(a: PermissionMode, b: PermissionMode) -> PermissionMode {
    fn rank(m: &PermissionMode) -> u8 {
        match m {
            PermissionMode::Plan => 0,    // 最受限
            PermissionMode::Default => 1,
            PermissionMode::Auto => 2,    // 最宽松
            PermissionMode::Custom { .. } => 1,
        }
    }
    if rank(&a) <= rank(&b) { a } else { b }
}
```

这意味着：如果你在 `Plan`（只读）模式下启动 Coordinator，哪怕是 `worker` 步骤——它本身声明的是 `Full`（`Auto`）权限——也会以 `Plan` 模式运行。权限只能向下传递，不能向上提升。

在 `iris plan` 流水线中这个设计很自然：`explorer` 产品步骤和 `reviewer` 架构步骤只读且使用快速便宜的模型。只有 `worker` 实现步骤有写权限，而它只有在便宜步骤已经完成分析和设计之后才会运行。

## 并行 vs 串行：选择合适的执行模式

Coordinator 提供两种执行模式：

**`run()` — 并行扇出。** 所有子任务作为独立的 Tokio task 并发运行，结果按提交顺序收集。适合任务之间相互独立的场景："搜索 auth 模块"、"搜索数据库层"、"检查测试覆盖率"可以同时跑，最后有一个可选的 synthesis agent 汇总所有结果。

**`pipeline_run()` — 串行+上下文注入。** 每一步等上一步完成后才运行，并在 system prompt 里收到所有先前步骤的输出。适合步骤之间有逻辑依赖的场景：不理解需求就写不出架构，没有架构就写不出代码。

两种模式都遵守 `CoordinatorConfig`：

```rust
pub struct CoordinatorConfig {
    pub max_threads: usize,  // 默认 6
    pub max_depth: u8,       // 默认 1
}
```

`max_depth: 1` 意味着子 Agent 自己不能再派生子 Agent。这防止了指数级扩张：depth 0 的 Coordinator 可以启动子 Agent，但这些子 Agent 自己不能再作为 Coordinator 使用。检查在进入时立即执行：

```rust
if self.depth >= self.config.max_depth {
    bail!("coordinator depth limit reached (max_depth={})", self.config.max_depth);
}
```

## MessageBus：流水线之外的点对点通信

对于无法在流水线移交模式中预先规划的通信需求——比如临时委托子任务、汇报中间发现、向同伴 Agent 请求澄清——`MessageBus` 提供了独立的通道：

```rust
pub struct BusMessage {
    pub from: String,   // 发送方 agent ID，例如 "pipe-0"
    pub to: String,     // 接收方 ID，"*" 表示广播
    pub content: String,
}
```

Bus 是一个 `tokio::sync::broadcast` channel 包在 `Arc` 里，同一个 Coordinator 下运行的所有 Agent 共享同一个底层通道。每个 Agent 的工具注册表里都有一个 `SendMessageTool`，绑定到共享 bus，Agent 可以像调用其他工具一样调用它：`send_message(to="pipe-2", content="...")`。

`MessageBus` 是结构化上下文注入的补充，而不是替代。上下文注入负责流水线步骤之间有序、结构化的移交；bus 负责无法预先规划的、无序的点对点协调。

## CLI 语法

`iris run --pipeline` 标志直接从命令行暴露 `pipeline_run()`：

```
iris run --pipeline \
  --sub "product@explorer:分析 auth 需求" \
  --sub "arch@reviewer:设计技术架构" \
  --sub "impl@worker:写实现代码和测试"
```

步骤格式为 `label@agent_type:prompt`，`@agent_type` 部分可以省略，省略后使用 Coordinator 的默认模型，不附加任何 AgentDefinition。

`iris plan` 子命令是这个模式的便捷封装，内置了针对产品分析、架构设计、实现生成各自调优的提示词：

```
iris plan "为 REST API 添加 JWT 认证"
iris plan "为 REST API 添加 JWT 认证" --arch-only
```

## 小结

这套移交协议的核心洞察是：**把结构化 Markdown 注入 system prompt** 是比共享状态或用户消息拼接都更好的原语。它让每个下游 Agent 对所有先前工作有类型化、有标签、有结构的访问，同时不污染用户消息，不需要文件系统层面的约定，也不会丢失 Rust 类型系统对移交接口的保障。权限级联确保廉价的只读 Agent 先完成分析和设计工作，只有特权 worker Agent 才能做出变更——而且只有在它拥有所需的完整上下文之后才会运行。
