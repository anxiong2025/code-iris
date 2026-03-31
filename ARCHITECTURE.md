# Code Iris — 架构设计文档

> 严格参考 Claude Code (v2.1.87) 源码架构，用 Rust 从零重构
>
> Claude Code 原始规模：1,884 个 TypeScript 文件，512,664 行代码
> Code Iris 目标：提取核心架构模式，用 Rust 实现精简但完整的版本

---

# 目录

- [设计原则](#设计原则)
- [与 Claude Code 的架构映射](#与-claude-code-的架构映射)
- [Crate 结构总览](#crate-结构总览)
- [iris-core — 核心引擎](#iris-core--核心引擎)
  - [Agent Loop (参考 QueryEngine)](#agent-loop-参考-queryengine)
  - [Tool System (参考 src/tools/)](#tool-system-参考-srctools)
  - [Scanner (参考 code-robin scanner.py)](#scanner-参考-code-robin-scannerpy)
  - [Reporter (参考 code-robin reporter.py)](#reporter-参考-code-robin-reporterpy)
  - [Config (参考 code-robin config.py)](#config-参考-code-robin-configpy)
  - [Storage (参考 Claude Code transcript)](#storage-参考-claude-code-transcript)
- [iris-llm — LLM 提供商层](#iris-llm--llm-提供商层)
  - [Provider Trait 设计](#provider-trait-设计)
  - [SSE 流解析器](#sse-流解析器)
  - [Anthropic 实现](#anthropic-实现)
  - [OpenAI Compatible 实现](#openai-compatible-实现)
  - [Google Gemini 实现](#google-gemini-实现)
- [iris-tui — 终端界面](#iris-tui--终端界面)
  - [UI 架构 (参考 React+Ink → Ratatui)](#ui-架构-参考-reactink--ratatui)
  - [组件设计](#组件设计)
  - [流式 Markdown 渲染](#流式-markdown-渲染)
- [iris-cli — 纯命令行](#iris-cli--纯命令行)
- [安全架构](#安全架构)
- [与 Claude Code 的差异决策](#与-claude-code-的差异决策)
- [开发阶段规划](#开发阶段规划)
- [附录：Claude Code 源码参考索引](#附录claude-code-源码参考索引)

---

# 设计原则

1. **参考而非复刻** — 提取 Claude Code 的核心架构模式（tool-call loop、流式处理、权限模型），但不盲目复制 51 万行代码的所有细节。
2. **Rust 原生** — 不是 "用 Rust 写 TypeScript"，而是利用 Rust 的所有权、trait、async 生态写出地道的 Rust 代码。
3. **安全第一** — 零 npm 依赖、rustls-tls、cargo audit/vet、API key 加密存储。
4. **模块解耦** — core 和 llm 是纯库 crate，不依赖任何 UI 框架。TUI/CLI/Desktop 三个前端共享同一套核心。
5. **渐进式构建** — 从能跑的最小版本开始，逐步添加功能。

---

# 与 Claude Code 的架构映射

```
Claude Code (TypeScript/Bun)              Code Iris (Rust)
═══════════════════════════════════════════════════════════════

入口层
  src/main.tsx                        →   iris-tui/src/main.rs
  Commander.js CLI                    →   clap derive
  React/Ink 渲染器                     →   Ratatui + crossterm

查询引擎
  src/services/QueryEngine.ts         →   iris-core/src/agent.rs
  (46,630 行)                              (目标 ~2,000 行)
  流式 API 调用                        →   reqwest + eventsource-stream
  Tool-call 循环                       →   async loop + tokio::select!
  5 级上下文压缩                       →   简化为 2 级 (truncate + summary)
  3 级输出恢复                         →   简化为 1 级 (auto-continue)

工具系统
  src/tools/ (~40 个)                 →   iris-core/src/tools/ (6 个核心)
  Zod schema 校验                     →   serde + 编译期类型检查
  权限模型 (4 种模式)                   →   简化为 2 种 (confirm / auto)
  流式工具执行器                       →   tokio::spawn + channel

命令系统
  src/commands/ (~50 个)              →   Phase 2+ (按需添加)
  /commit, /review 等                 →   scan, arch, deps, stats + chat

LLM 调用
  @anthropic-ai/sdk                   →   iris-llm/src/anthropic.rs (raw HTTP)
  OpenAI compat (httpx)               →   iris-llm/src/openai.rs (reqwest)
  无 Google 支持                       →   iris-llm/src/google.rs (新增)

服务层
  src/services/mcp.ts                 →   Phase 3+ (MCP 集成)
  src/services/lsp.ts                 →   Phase 3+ (LSP 集成)
  src/services/oauth.ts               →   暂不需要
  src/bridge/ (IDE)                   →   Phase 4+ (Tauri 替代)

基础设施
  src/hooks/                          →   iris-core 内置
  src/utils/memoize                   →   标准 HashMap 缓存
  src/state/                          →   iris-core/src/storage.rs
  GrowthBook 特性开关                  →   编译期 feature flag (Cargo features)
```

---

# Crate 结构总览

```
code-iris/
├── Cargo.toml                    # workspace 定义
│
├── crates/
│   ├── iris-core/                # 核心引擎 (无 UI 依赖)
│   │   └── src/
│   │       ├── lib.rs            # 公共 API
│   │       ├── agent.rs          # Agent Loop (对标 QueryEngine)
│   │       ├── scanner.rs        # 代码扫描 (tree-sitter)
│   │       ├── reporter.rs       # 报告生成
│   │       ├── config.rs         # 配置管理
│   │       ├── models.rs         # 数据模型
│   │       ├── storage.rs        # 会话持久化
│   │       └── tools/            # 工具系统
│   │           ├── mod.rs        # Tool trait
│   │           ├── bash.rs       # Shell 执行
│   │           ├── file_read.rs  # 文件读取
│   │           ├── file_write.rs # 文件写入
│   │           ├── file_edit.rs  # 文件编辑
│   │           ├── grep.rs       # 内容搜索
│   │           └── glob.rs       # 文件查找
│   │
│   ├── iris-llm/                 # LLM 提供商抽象
│   │   └── src/
│   │       ├── lib.rs            # Provider trait + registry
│   │       ├── types.rs          # Message, ToolUse, StreamEvent
│   │       ├── sse.rs            # SSE 流解析器
│   │       ├── anthropic.rs      # Claude API
│   │       ├── openai.rs         # OpenAI 兼容族
│   │       └── google.rs         # Gemini API
│   │
│   ├── iris-tui/                 # 终端 UI (Ratatui)
│   │   └── src/
│   │       ├── main.rs           # 入口 + 事件循环
│   │       ├── app.rs            # 应用状态机
│   │       ├── ui.rs             # 布局渲染
│   │       ├── welcome.rs        # 欢迎面板
│   │       ├── chat.rs           # 对话面板
│   │       ├── input.rs          # 输入框组件
│   │       └── markdown.rs       # Markdown 渲染
│   │
│   └── iris-cli/                 # 纯 CLI (无 TUI)
│       └── src/
│           └── main.rs           # scan/arch/deps/stats
│
└── tests/                        # 集成测试
```

---

# iris-core — 核心引擎

## Agent Loop (参考 QueryEngine)

### Claude Code 原始架构

Claude Code 的 QueryEngine 是整个系统的心脏（46,630 行），核心是一个 **tool-call 驱动的状态机**：

```
Claude Code QueryEngine 状态机:

submitMessage(prompt)
  ├── processUserInput() → 斜杠命令、变更消息
  ├── recordTranscript() → 持久化
  └── query() 主循环 ─── while(true) ───┐
      ├── [1] 压缩管道 (5 级)            │
      ├── [2] callModel() 流式循环        │
      │   ├── 流式接收 LLM 响应           │
      │   ├── 收集 tool_use blocks        │
      │   ├── 流式工具执行器并行执行       │
      │   └── 实时轮询已完成工具结果       │
      ├── [3] 后处理与恢复                │
      ├── [4] 工具结果收集                │
      ├── [5] 附件（记忆、技能）           │
      ├── [6] turnCount++ & 限制检查      │
      └── needsFollowUp? ────────────────┘
```

### Code Iris 简化设计

我们提取 QueryEngine 的核心模式，简化为 ~2,000 行 Rust：

```rust
/// Agent Loop — 核心状态机
///
/// 参考: Claude Code QueryEngine.ts
/// 简化: 去掉技能系统、特性开关、IDE bridge，保留核心循环
pub struct AgentLoop {
    /// 对话历史 (对标 QueryEngine.mutableMessages)
    messages: Vec<Message>,

    /// LLM 提供商 (对标 QueryEngine 的 API client)
    provider: Box<dyn LlmProvider>,

    /// 可用工具注册表 (对标 QueryEngine.tools)
    tools: ToolRegistry,

    /// 累积 token 用量 (对标 QueryEngine.totalUsage)
    usage: TokenUsage,

    /// 会话配置
    config: AgentConfig,
}

/// AgentConfig — 执行限制
///
/// 参考: Claude Code QueryEngineConfig
struct AgentConfig {
    /// 最大循环轮次 (对标 maxTurns)
    max_turns: usize,

    /// 最大预算 (对标 maxBudgetUsd)
    max_budget_usd: Option<f64>,

    /// 系统提示词
    system_prompt: String,

    /// 上下文窗口大小 (用于压缩触发)
    context_window: usize,
}
```

### 核心循环伪代码

```rust
impl AgentLoop {
    /// 主入口 — 返回流式事件
    ///
    /// 参考: QueryEngine.submitMessage()
    pub async fn run(&mut self, user_input: &str) -> Result<AgentResult> {
        self.messages.push(Message::user(user_input));

        let mut turn_count = 0;

        loop {
            // [1] 上下文压缩 (简化版，对标 QueryEngine 的 5 级管道)
            //     - 仅实现 truncate + auto-summary 两级
            self.compress_if_needed().await?;

            // [2] 调用 LLM (对标 QueryEngine.callModel)
            //     - 流式接收，实时 yield 文本事件
            let response = self.provider.chat_stream(&self.messages).await?;

            // [3] 解析响应
            let mut has_tool_use = false;
            let mut tool_calls = Vec::new();

            for event in response {
                match event {
                    StreamEvent::TextDelta(text) => {
                        // yield 给 UI 层实时渲染
                    }
                    StreamEvent::ToolUse(call) => {
                        has_tool_use = true;
                        tool_calls.push(call);
                    }
                    StreamEvent::Usage(u) => {
                        self.usage.accumulate(u);
                    }
                }
            }

            // [4] 执行工具 (对标 QueryEngine 的流式工具执行器)
            //     Claude Code 在 LLM 流式输出期间就开始并行执行工具
            //     我们简化为：收集完所有 tool_use 后并行执行
            if has_tool_use {
                let results = self.execute_tools(tool_calls).await?;
                for result in results {
                    self.messages.push(Message::tool_result(result));
                }
            }

            // [5] 限制检查 (对标 QueryEngine 的 turnCount + budget 检查)
            turn_count += 1;
            if turn_count >= self.config.max_turns {
                return Ok(AgentResult::MaxTurns);
            }

            // [6] 循环判断 (对标 QueryEngine.needsFollowUp)
            if !has_tool_use {
                return Ok(AgentResult::Success);
            }
        }
    }
}
```

### 上下文压缩 (简化版)

Claude Code 有 5 级压缩管道，我们简化为 2 级：

```
Claude Code 5 级:                    Code Iris 2 级:
1. 内容替换（裁剪超大工具结果）   →   Level 1: Truncate
2. Snip compact（删除旧消息）         - 裁剪超大工具结果 (>10KB → 摘要)
3. Microcompact（缓存编辑）           - 删除最旧的消息对
4. Context collapse（上下文折叠）
5. Autocompact（完整摘要压缩）    →   Level 2: Summary
                                      - 调用 LLM 生成对话摘要
                                      - 替换所有旧消息为摘要
```

### 错误恢复 (简化版)

Claude Code 有 3 级输出恢复和模型降级，我们简化为：

```
Claude Code:                          Code Iris:
1. Slot 升级 (8k→64k)            →   自动使用最大 output tokens
2. 多轮恢复 (inject resume)       →   1 次 auto-continue
3. 模型降级 (fallback model)      →   用户手动切换
```

---

## Tool System (参考 src/tools/)

### Claude Code 工具架构

Claude Code 有 ~40 个工具，每个工具是自包含模块：

```typescript
// Claude Code 工具定义模式 (TypeScript)
interface Tool {
  name: string;
  description: string;
  inputSchema: ZodSchema;        // Zod 校验
  permissionMode: PermissionMode; // 权限模型
  execute(input): Promise<ToolResult>;
  getProgressState?(): ProgressState;
}
```

### Code Iris Rust 实现

```rust
/// Tool trait — 所有工具的统一接口
///
/// 参考: Claude Code src/tools/ 的工具定义模式
/// Zod schema → serde 编译期类型检查
/// PermissionMode → ToolPermission enum
#[async_trait]
pub trait Tool: Send + Sync {
    /// 工具名称 (LLM 调用时使用)
    fn name(&self) -> &str;

    /// 工具描述 (发送给 LLM 的 tool description)
    fn description(&self) -> &str;

    /// JSON Schema (发送给 LLM 的 input_schema)
    fn input_schema(&self) -> serde_json::Value;

    /// 权限级别
    fn permission(&self) -> ToolPermission;

    /// 执行工具
    async fn execute(&self, input: serde_json::Value) -> Result<ToolResult>;
}

/// 权限模型
///
/// 参考: Claude Code src/hooks/toolPermission/ 的 4 种模式
/// 简化为 2 种
pub enum ToolPermission {
    /// 自动执行，无需确认 (读操作)
    Auto,
    /// 需要用户确认 (写操作、Shell 执行)
    Confirm,
}

/// 工具注册表
///
/// 参考: Claude Code QueryEngineConfig.tools
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}
```

### 工具清单与 Claude Code 对照

```
Claude Code (~40 tools)               Code Iris Phase 1 (6 tools)
───────────────────────────────────────────────────────────────
BashTool                           →   bash.rs ✓
FileReadTool                       →   file_read.rs ✓
FileWriteTool                      →   file_write.rs ✓
FileEditTool                       →   file_edit.rs ✓
GrepTool                           →   grep.rs ✓
GlobTool                           →   glob.rs ✓
AgentTool (子智能体)                →   Phase 2
SendMessageTool (智能体间通信)      →   Phase 2
WebSearchTool                      →   Phase 2
WebFetchTool                       →   Phase 2
MCPTool                            →   Phase 3
LSPTool                            →   Phase 3
TaskCreate/Update/List/Get         →   Phase 2
EnterPlanModeTool                  →   Phase 2
NotebookEditTool                   →   不实现 (非核心)
```

### BashTool 详细设计

```rust
/// BashTool — Shell 命令执行
///
/// 参考: Claude Code BashTool
/// - tokio::Command 替代 Node child_process
/// - 超时控制 (默认 120s, 最大 600s)
/// - stdout/stderr 捕获
/// - 工作目录保持
pub struct BashTool {
    working_dir: PathBuf,
    default_timeout: Duration,
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str { "bash" }

    fn permission(&self) -> ToolPermission {
        ToolPermission::Confirm  // Shell 命令需要确认
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolResult> {
        let command: String = serde_json::from_value(input["command"].clone())?;
        let timeout_ms: u64 = input["timeout"].as_u64().unwrap_or(120_000);

        let output = tokio::time::timeout(
            Duration::from_millis(timeout_ms),
            tokio::process::Command::new("sh")
                .arg("-c")
                .arg(&command)
                .current_dir(&self.working_dir)
                .output()
        ).await??;

        Ok(ToolResult {
            content: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            exit_code: output.status.code(),
        })
    }
}
```

---

## Scanner (参考 code-robin scanner.py)

### 原始 Python 实现

code-robin 用 Python `ast` 模块扫描 Python 文件，提取模块结构和依赖关系。

### Rust 重构方案

```rust
/// Scanner — 多语言代码扫描
///
/// 参考: code-robin scanner.py (Python ast 模块)
/// 升级: tree-sitter 支持多语言 AST 解析
///
/// code-robin:
///   - ast.parse() → ast.walk() → 提取 Import/ImportFrom
///   - 仅支持 Python
///
/// Code Iris:
///   - tree-sitter parse → query → 提取 imports/classes/functions
///   - 支持 Python, Rust, TypeScript, JavaScript, Go
pub struct Scanner {
    languages: HashMap<String, tree_sitter::Language>,
}

/// 扫描结果
///
/// 参考: code-robin ProjectManifest
pub struct ScanResult {
    pub root: PathBuf,
    pub modules: Vec<Module>,
    pub stats: ProjectStats,
    pub dependencies: Vec<Dependency>,
}

impl Scanner {
    /// 扫描项目
    ///
    /// 参考: code-robin scan_project() + scan_dependencies()
    /// 合并为单次扫描，避免重复遍历文件系统
    pub fn scan(&self, root: &Path) -> Result<ScanResult> {
        let files = self.discover_files(root)?;
        let mut modules = Vec::new();
        let mut dependencies = Vec::new();

        for file in &files {
            let lang = self.detect_language(file)?;
            let source = std::fs::read_to_string(file)?;
            let tree = self.parse(&source, lang)?;

            // tree-sitter query 提取结构信息
            modules.push(self.extract_module(file, &tree)?);
            dependencies.extend(self.extract_dependencies(file, &tree)?);
        }

        Ok(ScanResult { root: root.to_path_buf(), modules, stats, dependencies })
    }
}
```

### 多语言支持矩阵

```
语言         tree-sitter crate        提取内容
──────────────────────────────────────────────────
Python       tree-sitter-python       imports, classes, functions, decorators
Rust         tree-sitter-rust         use statements, mod, struct, impl, fn
TypeScript   tree-sitter-typescript   imports, classes, interfaces, functions
JavaScript   tree-sitter-javascript   imports, require(), classes, functions
Go           tree-sitter-go           imports, structs, interfaces, functions (Phase 2)
```

---

## Reporter (参考 code-robin reporter.py)

```rust
/// Reporter — 架构报告生成
///
/// 参考: code-robin Reporter class
/// 保持相同的 Markdown 输出格式，增加更多报告类型
pub struct Reporter {
    scan_result: ScanResult,
}

impl Reporter {
    /// 参考: code-robin Reporter.from_path()
    pub fn from_path(path: &Path) -> Result<Self>;

    /// 参考: code-robin render_manifest()
    pub fn render_manifest(&self) -> String;

    /// 参考: code-robin render_dependencies()
    pub fn render_dependencies(&self) -> String;

    /// 参考: code-robin render_stats()
    pub fn render_stats(&self) -> String;

    /// 参考: code-robin render_full_report()
    pub fn render_full_report(&self) -> String;

    /// 新增: 依赖关系图 (Mermaid 格式)
    pub fn render_dependency_graph(&self) -> String;

    /// 新增: 复杂度分析
    pub fn render_complexity(&self) -> String;
}
```

---

## Config (参考 code-robin config.py)

```rust
/// Config — 配置管理
///
/// 参考: code-robin config.py
/// 升级:
///   - ~/.code-robin/.env → ~/.code-iris/config.toml
///   - API key 加密存储 (secrecy crate)
///   - 多 profile 支持

/// 配置文件路径: ~/.code-iris/
///   ├── config.toml          # 主配置
///   ├── sessions/            # 会话历史
///   └── keys/                # 加密的 API keys

pub struct IrisConfig {
    /// 默认提供商 (对标 code-robin DEFAULT_PROVIDER)
    pub default_provider: String,

    /// 默认模型
    pub default_model: Option<String>,

    /// 提供商配置列表
    pub providers: Vec<ProviderEntry>,

    /// Agent 配置
    pub agent: AgentConfig,
}
```

---

## Storage (参考 Claude Code transcript)

```rust
/// Storage — 会话持久化
///
/// 参考: Claude Code 的 transcript 系统
///   - Claude Code 用 recordTranscript() 保存每次对话
///   - 支持 --resume 恢复上次会话
///
/// Code Iris 实现:
///   - JSON 格式存储对话历史
///   - ~/.code-iris/sessions/{session_id}.json
///   - 支持 list / resume / export

pub struct SessionStore {
    base_dir: PathBuf,  // ~/.code-iris/sessions/
}

impl SessionStore {
    /// 保存当前会话
    pub fn save(&self, session: &Session) -> Result<()>;

    /// 恢复会话 (对标 Claude Code --resume)
    pub fn load(&self, session_id: &str) -> Result<Session>;

    /// 列出最近会话
    pub fn list_recent(&self, limit: usize) -> Result<Vec<SessionSummary>>;

    /// 导出为 Markdown
    pub fn export_markdown(&self, session_id: &str) -> Result<String>;
}
```

---

# iris-llm — LLM 提供商层

## Provider Trait 设计

```rust
/// LLM Provider trait — 统一的模型接口
///
/// 参考: code-robin providers.py ProviderConfig + chat_completion
/// 升级:
///   - 同步 → 异步流式
///   - 纯文本 → 支持 tool_use 协议
///   - 无类型 → 强类型 StreamEvent

#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// 提供商名称
    fn name(&self) -> &str;

    /// 流式对话 (核心方法)
    ///
    /// 参考: Claude Code QueryEngine.callModel()
    /// 返回 StreamEvent 流，包含 text_delta / tool_use / usage
    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        config: &ModelConfig,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>>;

    /// 非流式对话 (便捷方法，用于摘要压缩等内部调用)
    async fn chat(
        &self,
        messages: &[Message],
        config: &ModelConfig,
    ) -> Result<String>;
}

/// 模型配置
pub struct ModelConfig {
    pub model: String,
    pub max_tokens: u32,
    pub temperature: Option<f32>,
    pub system_prompt: Option<String>,
}

/// 流式事件
///
/// 参考: Claude Code 流式处理的 3 种事件类型
pub enum StreamEvent {
    /// 文本增量 (对标 content_block_delta / text_delta)
    TextDelta(String),

    /// 思考增量 (对标 thinking delta — Claude extended thinking)
    ThinkingDelta(String),

    /// 工具调用 (对标 tool_use content block)
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },

    /// Token 用量 (对标 message_start.usage + message_delta.usage)
    Usage {
        input_tokens: u32,
        output_tokens: u32,
    },

    /// 消息结束
    MessageStop,
}
```

## SSE 流解析器

```rust
/// SSE Parser — Server-Sent Events 解码
///
/// 所有 LLM 提供商都使用 SSE 协议，格式：
///   event: message_start
///   data: {"type": "message_start", ...}
///
/// 参考: Claude Code 在 callModel() 中的流式处理逻辑
///
/// 实现: 使用 eventsource-stream crate 解析 reqwest::Response
///       然后根据提供商特定格式转换为统一的 StreamEvent

pub async fn parse_sse_stream(
    response: reqwest::Response,
    parser: impl Fn(&str, &str) -> Option<StreamEvent>,
) -> impl Stream<Item = Result<StreamEvent>> {
    // eventsource-stream 解析 SSE
    // parser 函数根据 event type + data 转换为 StreamEvent
}
```

## Anthropic 实现

```rust
/// Anthropic Claude provider
///
/// 参考: Claude Code 使用 @anthropic-ai/sdk
///       code-robin providers.py _chat_anthropic()
///
/// API: POST https://api.anthropic.com/v1/messages
/// 流式: SSE with event types:
///   - message_start → 初始 usage
///   - content_block_start → text / tool_use block 开始
///   - content_block_delta → text_delta / input_json_delta
///   - content_block_stop
///   - message_delta → stop_reason, final usage
///   - message_stop

pub struct AnthropicProvider {
    api_key: secrecy::SecretString,
    client: reqwest::Client,
    base_url: String,  // 默认 https://api.anthropic.com
}
```

## OpenAI Compatible 实现

```rust
/// OpenAI-compatible provider
///
/// 参考: code-robin providers.py _chat_openai_compat()
///
/// 覆盖所有 OpenAI 兼容的提供商 (同一套代码，不同 base_url):
///   - OpenAI:      https://api.openai.com/v1
///   - DeepSeek:    https://api.deepseek.com/v1
///   - Ollama:      http://localhost:11434/v1
///   - OpenRouter:  https://openrouter.ai/api/v1
///   - Groq:        https://api.groq.com/openai/v1
///   - 通义千问:     https://dashscope.aliyuncs.com/compatible-mode/v1
///   - 智谱 AI:     https://open.bigmodel.cn/api/paas/v4
///   - 月之暗面:     https://api.moonshot.cn/v1
///   - 硅基流动:     https://api.siliconflow.cn/v1
///   - 以及更多...
///
/// API: POST {base_url}/chat/completions
/// 流式: SSE with data: {"choices": [{"delta": {...}}]}

pub struct OpenAiCompatProvider {
    api_key: secrecy::SecretString,
    client: reqwest::Client,
    base_url: String,
    provider_name: String,
}
```

## Google Gemini 实现

```rust
/// Google Gemini provider
///
/// 新增 (code-robin 未支持)
///
/// API: POST https://generativelanguage.googleapis.com/v1beta/models/{model}:streamGenerateContent
/// 流式: SSE with candidates[0].content.parts

pub struct GoogleProvider {
    api_key: secrecy::SecretString,
    client: reqwest::Client,
}
```

---

# iris-tui — 终端界面

## UI 架构 (参考 React+Ink → Ratatui)

### Claude Code 的 UI 架构

```
Claude Code UI (React + Ink):
  - 声明式组件模型 (JSX)
  - React 状态管理 (useState, useReducer)
  - Ink 渲染器 (将 React 虚拟 DOM 映射到终端)
  - Flexbox 布局
```

### Code Iris 的 UI 架构

```
Code Iris UI (Ratatui + crossterm):
  - Immediate-mode 渲染 (每帧重绘)
  - 集中式状态 (App struct)
  - Ratatui 渲染器 (Widget trait)
  - Constraint-based 布局
```

### 事件循环

```rust
/// 应用事件循环
///
/// 参考: Claude Code React/Ink 的渲染循环
/// Ratatui 使用 immediate-mode: 每次事件都完整重绘
///
/// Claude Code:
///   React state change → Ink reconciler → 终端 diff → 最小化更新
///
/// Code Iris:
///   Event → update App state → terminal.draw(|f| ui(f, &app)) → 全帧渲染

pub async fn run_app(config: AppConfig) -> Result<()> {
    let mut terminal = ratatui::init();
    let mut app = App::new(config);

    loop {
        // 渲染
        terminal.draw(|frame| ui::render(frame, &app))?;

        // 事件处理 (键盘 + LLM 流式事件)
        tokio::select! {
            // 终端输入事件
            event = crossterm_event() => {
                app.handle_key_event(event?);
            }
            // LLM 流式响应事件
            stream_event = app.next_stream_event() => {
                if let Some(event) = stream_event {
                    app.handle_stream_event(event);
                }
            }
        }

        if app.should_quit() {
            break;
        }
    }

    ratatui::restore();
    Ok(())
}
```

## 组件设计

```
┌─────────────────────────────────────────────────────────────┐
│  Code Iris v0.1.0                                           │
├──────────────────────────┬──────────────────────────────────┤
│                          │  Tips                            │
│  Welcome back!           │  Run `scan .` to analyze...      │
│                          ├──────────────────────────────────┤
│  ⦿ ASCII Art / Logo      │  Recent activity                 │
│                          │  No recent activity              │
│  Provider · Model        │                                  │
│  ~/current/path          │                                  │
├──────────────────────────┴──────────────────────────────────┤
│                                                             │
│  [对话历史区域 — 滚动]                                       │
│                                                             │
│  user> scan this project                                    │
│  iris> Scanning... found 42 files                           │
│        [tool: bash] ls -la                                  │
│        [tool: file_read] src/main.rs                        │
│        Here is the architecture report: ...                 │
│                                                             │
├─────────────────────────────────────────────────────────────┤
│ ❯ █                                                         │
├─────────────────────────────────────────────────────────────┤
│                              claude-sonnet-4.6 · tokens: 1.2k │
└─────────────────────────────────────────────────────────────┘

组件映射:
  WelcomePanel  → welcome.rs  (参考 Claude Code 启动画面)
  ChatPanel     → chat.rs     (参考 Claude Code 对话区域)
  InputBox      → input.rs    (参考 Claude Code 输入框)
  StatusBar     → ui.rs       (参考 Claude Code 底部状态栏)
  MarkdownView  → markdown.rs (流式 Markdown 渲染)
```

## 流式 Markdown 渲染

```rust
/// Markdown 渲染器
///
/// 参考: Claude Code 使用 Ink 组件渲染 Markdown
///       包括代码高亮、表格、列表等
///
/// Code Iris 使用:
///   - pulldown-cmark 解析 Markdown AST
///   - syntect 做代码语法高亮
///   - Ratatui Paragraph + Spans 渲染富文本
///
/// 关键: 支持流式渲染 — LLM 输出一个 token 就立即渲染
///       不等整个消息完成

pub struct MarkdownRenderer {
    highlighter: syntect::highlighting::Highlighter,
}

impl MarkdownRenderer {
    /// 渲染 Markdown 为 Ratatui 富文本
    pub fn render(&self, markdown: &str, width: u16) -> Vec<Line<'_>>;

    /// 增量渲染 — 追加新文本到已有渲染结果
    pub fn render_delta(&mut self, delta: &str) -> Vec<Line<'_>>;
}
```

---

# iris-cli — 纯命令行

```rust
/// 纯 CLI — 无 TUI 依赖，脚本友好
///
/// 参考: code-robin main.py 的非交互模式
///
/// 命令:
///   iris scan [path]      扫描项目结构
///   iris arch [path]      生成架构报告
///   iris deps [path]      分析依赖关系
///   iris stats [path]     输出统计信息
///   iris configure        配置 API keys
///   iris models            列出可用模型

#[derive(Parser)]
#[command(name = "iris", about = "See through your code")]
enum Cli {
    Scan { path: Option<PathBuf> },
    Arch { path: Option<PathBuf>, #[arg(short, long)] output: Option<PathBuf> },
    Deps { path: Option<PathBuf> },
    Stats { path: Option<PathBuf> },
    Configure,
    Models,
}
```

---

# 安全架构

## 供应链安全

```
措施                                  对标
─────────────────────────────────────────────────
零 npm 依赖                        vs Claude Code 整个 npm 生态
rustls-tls (纯 Rust TLS)           vs Node.js 依赖系统 OpenSSL
cargo audit (RustSec 漏洞库)        vs npm audit (漏报多)
cargo vet (Mozilla 依赖审查)         vs 无对标
cargo deny (许可证+来源检查)          vs 无对标
无 postinstall 脚本机制              vs npm postinstall 攻击面
静态链接单二进制                     vs 需要 Bun/Node 运行时
```

## API Key 安全

```rust
/// API Key 存储
///
/// 参考: code-robin config.py 明文存储 .env
/// 升级: secrecy crate 内存保护 + 可选 keyring 系统密钥链

// code-robin (不安全):
//   ANTHROPIC_API_KEY=sk-ant-xxx   ← 明文在 .env 文件

// Code Iris:
//   - 内存中: SecretString (实现 Zeroize，释放时清零)
//   - 磁盘上: config.toml 中存储 (可选加密)
//   - 系统级: macOS Keychain / Linux Secret Service (Phase 2)
```

## 工具执行沙箱

```rust
/// 权限控制
///
/// 参考: Claude Code src/hooks/toolPermission/
///
/// Claude Code 4 种模式: default, plan, bypassPermissions, auto
/// Code Iris 简化为 2 种: Confirm, Auto
///
/// 所有写操作和 Shell 执行默认需要用户确认
/// 读操作自动通过
```

---

# 与 Claude Code 的差异决策

| 决策 | Claude Code | Code Iris | 理由 |
|------|------------|-----------|------|
| 语言 | TypeScript | Rust | 安全、性能、单二进制分发 |
| 运行时 | Bun | 无 (native) | 零运行时依赖 |
| UI 框架 | React + Ink | Ratatui | Rust 生态最成熟的 TUI |
| LLM SDK | @anthropic-ai/sdk | raw reqwest | 减少依赖，完全控制 |
| 压缩管道 | 5 级 | 2 级 | 80/20 法则，核心功能足够 |
| 输出恢复 | 3 级升级 | 1 级 auto-continue | 简化复杂度 |
| 权限模式 | 4 种 | 2 种 | 足够覆盖读/写场景 |
| 工具数量 | ~40 | 6 (Phase 1) | 渐进式添加 |
| 命令数量 | ~50 | 6 (Phase 1) | 聚焦核心功能 |
| 特性开关 | GrowthBook (运行时) | Cargo features (编译期) | 编译期消除，零运行时开销 |
| IDE 集成 | VS Code + JetBrains bridge | Phase 4 (Tauri) | 先做好终端版 |
| MCP | 完整实现 | Phase 3 | 核心功能优先 |
| 多智能体 | 完整协调器 | Phase 2 (AgentTool) | 渐进式构建 |
| 代码分析 | 无 (通用 agent) | tree-sitter 多语言 | Code Iris 的差异化价值 |

---

# 开发阶段规划

## Phase 1: Foundation (4-6 周)

```
目标: 可用的 CLI + 基础 LLM 对话

iris-core:
  ✓ models.rs — 数据模型定义
  ✓ config.rs — dotenv/TOML 配置加载
  ✓ scanner.rs — tree-sitter Python 扫描
  ✓ reporter.rs — Markdown 报告生成

iris-llm:
  ✓ types.rs — Message, StreamEvent
  ✓ sse.rs — SSE 流解析
  ✓ anthropic.rs — Claude API 流式调用
  ✓ openai.rs — OpenAI 兼容族

iris-cli:
  ✓ scan / arch / deps / stats 命令
  ✓ configure 命令
  ✓ models 命令

交付物: `cargo install code-iris` 可用
```

## Phase 2: Agent + TUI (6-8 周)

```
目标: 类 Claude Code 的终端交互体验

iris-core:
  ✓ agent.rs — tool-call 循环
  ✓ tools/ — 6 个核心工具
  ✓ storage.rs — 会话持久化

iris-tui:
  ✓ 欢迎面板 + 输入框
  ✓ 对话面板 + 流式渲染
  ✓ Markdown + 代码高亮
  ✓ 状态栏 (provider/model/tokens)

交付物: `code-iris` 终端 TUI 可用
```

## Phase 3: Ecosystem (4-6 周)

```
目标: 多语言支持 + 高级功能

iris-core:
  ✓ scanner — Rust/TS/JS/Go 支持
  ✓ tools — WebSearch, WebFetch, AgentTool
  ✓ MCP 客户端集成

iris-llm:
  ✓ google.rs — Gemini API
  ✓ Bedrock 支持 (feature flag)

交付物: 多语言代码分析 + MCP 集成
```

## Phase 4: Desktop (6-8 周)

```
目标: Tauri 桌面应用

iris-desktop:
  ✓ Tauri 2.0 + Web 前端
  ✓ 复用 iris-core 所有功能
  ✓ 文件拖放、系统通知
  ✓ 多标签会话

交付物: macOS / Windows / Linux 桌面应用
```

---

# 附录：Claude Code 源码参考索引

本文档的所有设计决策都参考了 Claude Code v2.1.87 的源码分析。以下是关键参考点：

```
Claude Code 文件                         参考用于 Code Iris
──────────────────────────────────────────────────────────────
src/main.tsx (803K 行)                   iris-tui/src/main.rs 启动流程
src/services/QueryEngine.ts (46,630 行)  iris-core/src/agent.rs 核心循环
src/services/query.ts (1,729 行)         iris-core/src/agent.rs 流式处理
src/tools/ (~40 tools)                   iris-core/src/tools/ 工具系统
src/hooks/toolPermission/                iris-core/src/tools/mod.rs 权限模型
src/commands/ (~50 commands)             iris-cli CLI 命令设计
src/services/mcp.ts                      Phase 3 MCP 集成参考
src/bridge/                              Phase 4 IDE 集成参考
src/memdir/                              iris-core/src/storage.rs 持久化参考
```

### Claude Code 关键架构模式在 Rust 中的实现

```
模式                    TypeScript 实现           Rust 实现
──────────────────────────────────────────────────────────
Tool-call 循环          while(true) + await       loop + tokio::select!
流式响应               AsyncGenerator/yield       Stream trait + async-stream
并行工具执行            Promise.all               tokio::join! / JoinSet
类型校验               Zod runtime 校验           serde 编译期 + runtime
特性开关               GrowthBook runtime         Cargo features 编译期
Memoization            memoize() 包装器           HashMap / once_cell
上下文压缩             5 级管道                   2 级 (truncate + summary)
错误恢复               3 级升级                   1 级 auto-continue
权限系统               4 种模式                   2 种 (Confirm / Auto)
```
