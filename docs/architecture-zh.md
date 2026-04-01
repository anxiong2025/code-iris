# Code Iris — 架构文档

> 版本：0.1.x · 更新日期：2026-04

## 概览

Code Iris 是一个原生 Rust 实现的 AI 编程助手，集成了流式 LLM 对话循环、多智能体编排层、14 工具执行环境，以及对三类 LLM 提供商的完整支持。项目提供两个入口：基于 `ratatui` 的终端交互界面（`code-iris`）和无界面命令行工具（`iris`）。

```
┌────────────────────────────────────────────────────────┐
│                      用户界面层                         │
│   iris-tui（ratatui TUI）  │   iris-cli（clap CLI）     │
└────────────────────────────────────────────────────────┘
                       │               │
                       ▼               ▼
┌────────────────────────────────────────────────────────┐
│                     iris-core                          │
│  ┌─────────────┐  ┌─────────────┐  ┌───────────────┐  │
│  │    Agent    │  │ Coordinator │  │  ToolRegistry  │  │
│  │  （对话循环）│  │（多智能体） │  │  （14 个工具） │  │
│  └─────────────┘  └─────────────┘  └───────────────┘  │
└────────────────────────────────────────────────────────┘
                             │
                             ▼
┌────────────────────────────────────────────────────────┐
│                     iris-llm                           │
│  Anthropic │ OpenAI 兼容接口 │ Google Gemini           │
└────────────────────────────────────────────────────────┘
```

## 工作区结构

| Crate | 职责 |
|-------|------|
| `iris-core` | 智能体逻辑、工具执行、多智能体编排、权限模型 |
| `iris-llm` | 提供商抽象层、SSE 流式传输、OAuth 与 API 密钥鉴权 |
| `iris-tui` | 基于 `ratatui` 的交互式终端界面 |
| `iris-cli` | 基于 `clap` 的无头 CLI，定义 `plan`、`doc-sync`、`run` 命令 |

## 智能体循环

`Agent::chat_streaming()` 驱动核心交互流程。

```
用户消息
     │
     ▼
┌──────────────────────────────────────┐
│  第 1 轮 … 第 20 轮                  │
│                                      │
│  1. 构建消息（系统提示 + 历史记录）   │
│  2. 通过 SSE 流式接收 LLM 响应       │◄── tokio::select!（取消标志）
│  3. 收集工具调用                     │
│  4. 在 ToolRegistry 中执行工具       │
│  5. 追加结果，进入下一轮             │
│                                      │
│  [无工具调用] → 返回给用户           │
└──────────────────────────────────────┘
```

关键设计要点：

- **20 轮上限**：防止循环失控，同时支持复杂的多步骤任务。
- **流式取消**：通过 `Arc<AtomicBool>` 与 `tokio::select!` 配合实现，允许在流式输出过程中随时中断，无需重建异步运行时。
- **四级上下文压缩**：当上下文窗口接近容量上限时，启用 LLM 辅助摘要（autocompact）。早期对话会被压缩为结构化摘要，保留任务状态，而非逐字保留所有 token。

## 多智能体编排：Coordinator

当任务超出单个智能体的处理能力时，`Coordinator` 负责管理智能体层级。

```
Coordinator
├─ ToolRegistry（共享）
├─ CoordinatorConfig { max_threads: 6, max_depth: 1 }
│
├─ run_subtasks(Vec<SubTask>)          ← 并行执行
│    最多同时启动 6 个智能体
│    收集输出 → 综合结果
│
└─ pipeline_run(Vec<PipelineStep>)     ← 串行执行
     第 N 步输出 → 结构化上下文 → 第 N+1 步
```

### 并行子智能体

`run_subtasks` 将一批 `SubTask` 分发给最多 `max_threads` 个并发智能体。所有子智能体完成后，结果被汇总并综合，再返回给父智能体。

### 串行流水线

`pipeline_run` 顺序执行各步骤。每一步的输出会以结构化系统上下文的形式注入到下一步：

```
# Prior step results
## product
<文本>
## architecture
<文本>
```

这种方式使后续步骤能够感知前置决策，而无需重放完整的对话历史。

### 权限上限机制

`most_restrictive(a, b) -> PermissionMode` 确保子智能体的权限始终不高于父智能体。权限从低到高依次为：`ReadOnly < Default < Plan < Auto < Custom(Full)`。`depth: u8` 守卫在达到 `max_depth` 时提前退出，防止无限递归委派。

## 智能体类型系统

智能体由 `AgentDefinition { name, description, instructions, model, sandbox_mode }` 描述。

`SandboxMode` 有两种模式：

| 变体 | 能力范围 |
|------|---------|
| `ReadOnly` | 读取文件、grep、glob、LSP 语义查询、网页抓取 |
| `Full` | 以上全部，加上 `bash`、`file_write`、`file_edit`、`task_*` |

三个内置智能体始终可用：

| 名称 | 模式 | 模型 |
|------|------|------|
| `explorer` | ReadOnly | Claude Haiku（轻量快速） |
| `worker` | Full | 配置的默认模型 |
| `reviewer` | ReadOnly | 配置的默认模型 |

自定义智能体从 `.iris/agents/*.toml`（项目级）和 `~/.code-iris/agents/*.toml`（用户全局级）加载。`find_agent(name, project_root)` 优先查找自定义定义，未找到时回退到内置智能体。这样团队可以定义专用智能体（例如携带特定指令的 `db-migration` 智能体），而无需修改可执行文件。

## 工具系统

`ToolRegistry` 中的 14 个工具按职责分组：

| 分组 | 工具 |
|------|------|
| 文件 I/O | `bash`、`file_read`、`file_write`、`file_edit`、`grep`、`glob` |
| 语义分析 | `lsp` |
| 网络 | `web_fetch`、`web_search` |
| 任务编排 | `task_create`、`task_update`、`task_list`、`task_get`、`agent_tool`、`send_message` |

所有 I/O 工具共享 `CwdRef = Arc<Mutex<Option<PathBuf>>>`，用于追踪智能体的当前工作目录，保证异步工具调用中相对路径的一致性。

`lsp` 工具通过标准输入输出上的 JSON-RPC 协议与语言服务器通信，支持 hover、跳转到定义、查找引用和诊断信息，为智能体提供超越文本匹配的语义代码理解能力。

`send_message` 向 `MessageBus` 发布消息，允许同一 `Coordinator` 下的兄弟智能体交换结构化数据，无需经过 LLM 中转。

## LLM 提供商层（`iris-llm`）

```
                  ┌───────────────────┐
                  │  LlmClient trait  │
                  └─────────┬─────────┘
           ┌────────────────┼────────────────┐
           ▼                ▼                ▼
    AnthropicClient   OpenAIClient    GeminiClient
    (SSE, OAuth+Key)  (17+ 提供商)   (SSE, 差分重建)
```

自动检测顺序：OAuth 令牌 → `ANTHROPIC_API_KEY` → 其他环境变量中的密钥。配置了 Claude OAuth 的开发者无需任何额外设置即可直接使用；CI 环境则通过环境变量覆盖。

Google Gemini 使用差分（delta diff）方式重建流式文本，以适配该提供商非标准的 SSE 格式。

## 权限模型

`PermissionMode` 变体：`Default`、`Plan`、`Auto`、`Custom`。

`most_restrictive()` 是权限在智能体层级中传播的唯一收口——每次 `Coordinator` 启动子智能体时都会调用它，从架构上杜绝了权限意外提升的可能。

## CLI 命令（`iris-cli`）

| 命令 | 说明 |
|------|------|
| `iris plan "提示词"` | 三步串行流水线：产品规格 → 架构设计 → 实现方案。`--arch-only` 跳过实现步骤。 |
| `iris doc-sync --since <ref>` | 对指定 git ref 执行 `git diff`，检测哪些 `.md` 章节已过时，并提出更新建议。 |
| `iris run --pipeline --sub "label@type:prompt"` | 执行临时流水线或子智能体调用。 |

TUI 内置斜杠命令（`/plan`、`/agents`、`/commit`、`/memory`、`/cd`、`/worktree` 等）以交互方式暴露相同的编排能力。

## 关键设计决策

**为什么选择 Rust？** 异步 Rust（`tokio`）使得基于 `tokio::select!` 的取消机制和并发子智能体扇出实现起来干净利落，没有回调地狱。Rust 的借用检查器在编译期强制约束了 `Arc<Mutex<CwdRef>>` 的共享使用规范。

**为什么用深度限制而非环检测？** 无环委派更易于推理和审计。对于当前的使用场景，`max_depth: 1` 是合理的默认值；对于理解其取舍的高级用户，该配置可以调整。

**`iris plan` 为什么用串行流水线而非并行？** 架构设计步骤需要以产品规格作为输入，实现步骤需要以架构设计作为输入，依赖关系是线性的。串行流水线加注入上下文既保证了正确性，也便于调试。

**为什么用 LLM 辅助的 autocompact 而非简单截断？** 简单截断会丢失任务状态。摘要压缩能够保留意图、约束条件和中间决策——这正是长时运行智能体保持连贯性所需要的。
