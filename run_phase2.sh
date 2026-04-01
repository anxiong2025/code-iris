#!/bin/bash
claude --dangerously-skip-permissions \
  --max-budget-usd 30.00 \
  --add-dir /Users/robert/project/2026/claude-code/src \
  "你是 Rust 专家，负责完成 code-iris Phase 2 的全部开发。

参考资料：
- Python 原版代码在 /Users/robert/project/2026/claude-code/src/（直接阅读参考）
- 架构设计文档：ARCHITECTURE.md（当前目录）
- 优化目标：OPTIMIZATIONS.md（当前目录）

工作目录：/Users/robert/project/2026/code-iris

当前状态：
- iris-llm (types/sse/anthropic/openai/google) 已完成，cargo check 通过
- iris-core/models.rs, config.rs, scanner.rs, reporter.rs 已完成
- iris-core/agent.rs, storage.rs, tools/* 只有注释占位符，需要实现
- iris-tui 骨架已存在（main.rs/app.rs/welcome.rs/input.rs/statusbar.rs），但未接入真实 Agent
- iris-cli/src/main.rs 只有 println! 占位符，需要实现完整 CLI

按顺序完成以下模块，每完成一个 cargo check 验证，有错误立即修复再继续：

1. crates/iris-core/src/tools/mod.rs
定义 Tool trait：
  pub trait Tool: Send + Sync {
      fn name(&self) -> &str;
      fn description(&self) -> &str;
      fn input_schema(&self) -> serde_json::Value;
      async fn execute(&self, input: serde_json::Value) -> anyhow::Result<String>;
  }
定义 ToolRegistry { tools: HashMap<String, Arc<dyn Tool>> }：
  pub fn new() -> Self
  pub fn register(&mut self, tool: Arc<dyn Tool>)
  pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>>
  pub fn all_definitions(&self) -> Vec<iris_llm::ToolDefinition>
  pub fn default_registry() -> Self（注册全部6个工具）
导出 pub use 所有工具模块。

2. crates/iris-core/src/tools/bash.rs
BashTool：执行 shell 命令，超时 30s，捕获 stdout+stderr，合并输出。
input schema: { command: String, timeout_seconds?: u64 }
用 tokio::process::Command，timeout 用 tokio::time::timeout。
输出格式：如果成功返回 stdout，如果失败返回 \"Exit {code}:\\n{stderr}\"。

3. crates/iris-core/src/tools/file_read.rs
FileReadTool：读取文件，支持行范围。
input schema: { path: String, start_line?: usize, end_line?: usize }
用 tokio::fs::read_to_string，输出带行号（cat -n 格式）。
最多返回 2000 行，超出时提示截断。

4. crates/iris-core/src/tools/file_write.rs
FileWriteTool：写入文件（覆盖），自动创建父目录。
input schema: { path: String, content: String }
用 tokio::fs::write，返回 \"Written {n} bytes to {path}\"。

5. crates/iris-core/src/tools/file_edit.rs
FileEditTool：精确字符串替换。
input schema: { path: String, old_string: String, new_string: String }
读取文件，确认 old_string 存在且唯一（出现多次报错），替换后写回。
返回替换成功/失败信息。

6. crates/iris-core/src/tools/grep.rs
GrepTool：用 grep 命令搜索文件内容。
input schema: { pattern: String, path?: String, file_glob?: String }
用 BashTool 执行 grep -rn --include=\"{glob}\" \"{pattern}\" \"{path}\"。
或者直接用 tokio::process::Command 执行 grep，返回匹配行。

7. crates/iris-core/src/tools/glob.rs
GlobTool：文件模式匹配。
input schema: { pattern: String, path?: String }
用 glob crate 展开 pattern，返回匹配文件路径列表（每行一个）。
结果按修改时间排序，最多返回 200 个。

8. crates/iris-core/src/storage.rs
Session 持久化，路径 ~/.code-iris/sessions/：
  pub struct Session { pub id: String, pub messages: Vec<iris_llm::Message>, pub created_at: u64, pub updated_at: u64 }
  pub struct Storage { dir: PathBuf }
  impl Storage:
    pub fn new() -> Result<Self>（创建目录）
    pub fn save(&self, session: &Session) -> Result<()>（写 JSON）
    pub fn load(&self, id: &str) -> Result<Session>（读 JSON）
    pub fn list(&self) -> Result<Vec<String>>（列出 session id）
    pub fn new_session() -> Session（生成 uuid id）
用 serde_json 序列化，文件名为 {id}.json。

9. crates/iris-core/src/agent.rs
Agent loop，对标 Claude Code QueryEngine：
  pub struct Agent {
      provider: AnthropicProvider,（或通用 provider，暂用 Anthropic）
      config: ModelConfig,
      tools: ToolRegistry,
      session: Session,
      storage: Storage,
  }
  impl Agent:
    pub fn new(api_key: impl Into<String>) -> Result<Self>
    pub async fn chat(&mut self, user_input: &str) -> Result<AgentResponse>
    pub async fn stream_chat(&mut self, user_input: &str, on_text: impl Fn(&str)) -> Result<AgentResponse>
  AgentResponse { text: String, tool_calls: Vec<String>, usage: TokenUsage }
  核心循环：
    1. 追加 user message 到 session
    2. 调用 provider.chat_stream()
    3. 收集 StreamEvent：累积 TextDelta，遇到 ToolUse 执行工具
    4. 工具结果追加为 tool_result message
    5. 如有工具调用则继续循环，否则结束
    6. 保存 session
  最多循环 10 次（防止无限循环）。

10. crates/iris-cli/src/main.rs
clap derive CLI，实现全部命令：
  scan [path]（默认当前目录，输出 manifest markdown）
  arch [path] [-o output.md]（输出完整报告，-o 写文件）
  deps [path]（输出依赖分析）
  stats [path]（输出统计表格）
  configure（交互式配置 API keys，调用 iris_core::config::configure_interactive）
  models（列出全部15个提供商，显示 ✓ 已配置 / ✗ 未配置）
  chat [--model model] [--session id]（启动 TUI，调用 iris-tui 的 run_tui 函数，或直接在 CLI inline chat）
实现 inline chat 模式（不启动 TUI）：循环读取 stdin，调用 Agent::chat，打印回复。

11. crates/iris-tui/src/app.rs（更新，接入真实 Agent）
更新 App struct 接入真实 Agent，支持异步消息处理：
- App { agent: Option<Agent>, ... }
- handle_command 改为异步，调用 agent.chat()
- 如果 ANTHROPIC_API_KEY 未设置，显示提示信息
注意：TUI 是同步的 ratatui 事件循环，用 tokio::runtime::Handle::current().block_on() 调用 async agent。

更新 iris-core/src/lib.rs 导出所有新模块。

验收标准（全部通过才算完成）：
1. cargo check --workspace 零错误零警告
2. cargo build -p iris-cli --release 成功
3. 找到 iris binary（在 ~/.cargo/global-target/release/iris）并运行 iris scan . 输出 manifest
4. 运行 iris models 列出全部15个提供商
5. 运行 iris deps . 输出依赖分析
6. 运行 iris stats . 输出统计表格

注意事项：
- 所有依赖用 workspace 定义，子 crate 只写 dep_name.workspace = true
- API key 必须用 secrecy::SecretString 保护（agent.rs 中）
- 所有错误用 anyhow::Result
- 不写无用 import，避免 dead_code 警告
- iris-tui 已有骨架，不要删除，只做最小改动接入真实 Agent
- Cargo.toml 里已有 glob crate，直接用
- 注意 iris-llm 的 AnthropicProvider 和 ModelConfig 已在 Phase 1 实现"
