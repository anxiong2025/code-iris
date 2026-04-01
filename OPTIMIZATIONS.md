# Code Iris — 针对 Claude Code 的优化项

> 基于 Claude Code v2.1.87 源码分析，识别架构和实现层面的可优化点
> 每项优化都标注了原始问题、Code Iris 的改进方案、以及预期收益

---

# 目录

- [P0: 架构层面优化](#p0-架构层面优化)
- [P1: 性能优化](#p1-性能优化)
- [P2: 安全优化](#p2-安全优化)
- [P3: 用户体验优化](#p3-用户体验优化)
- [P4: 可维护性优化](#p4-可维护性优化)
- [优化矩阵总览](#优化矩阵总览)

---

# P0: 架构层面优化

## OPT-01: QueryEngine 巨型文件拆分

**Claude Code 问题:**
```
src/services/QueryEngine.ts — 46,630 行（单文件）
src/main.tsx — 803K（单文件入口）
```
单文件承担了太多职责：LLM 调用、tool-call 循环、上下文压缩、错误恢复、Token 追踪、流式处理全部混在一起。修改任何一个子功能都需要理解整个文件。

**Code Iris 改进:**
```
iris-core/src/
├── agent.rs          # 纯粹的 agent 循环 (~300 行)
├── agent/
│   ├── loop.rs       # tool-call 循环状态机
│   ├── compress.rs   # 上下文压缩 (独立模块)
│   ├── recovery.rs   # 错误恢复策略
│   └── usage.rs      # Token 追踪与预算
```
每个子模块 200-500 行，职责单一，可独立测试。

**预期收益:** 可维护性提升，新贡献者能在 10 分钟内理解单个模块。

---

## OPT-02: 压缩管道过度工程

**Claude Code 问题:**
```
5 级压缩管道:
  1. 内容替换（裁剪超大工具结果）
  2. Snip compact（删除旧消息）      ← 特性开关控制
  3. Microcompact（缓存编辑）         ← 特性开关控制
  4. Context collapse（上下文折叠）    ← 特性开关控制
  5. Autocompact（完整摘要压缩）

+ 3 级恢复路径（当 prompt-too-long 时）
+ 多个特性开关交叉控制
```
5 级管道 + 3 级恢复 + 特性开关组合，形成了指数级的状态空间，极难调试。实际使用中大部分场景只触发第 1 级和第 5 级。

**Code Iris 改进:**
```rust
enum CompressionStrategy {
    /// Level 1: 裁剪超大工具结果 (>10KB → 前 2KB + "...truncated")
    /// 覆盖 Claude Code 第 1 级
    Truncate,

    /// Level 2: 调用 LLM 生成摘要，替换旧消息
    /// 合并 Claude Code 第 2-5 级为一步
    Summarize,
}

impl AgentLoop {
    fn compress_if_needed(&mut self) -> Result<()> {
        let usage_ratio = self.token_count() as f64 / self.config.context_window as f64;

        if usage_ratio > 0.9 {
            // 先尝试轻量级裁剪
            self.apply(CompressionStrategy::Truncate);

            // 如果还不够，做完整摘要
            if self.token_count_ratio() > 0.8 {
                self.apply(CompressionStrategy::Summarize).await?;
            }
        }
        Ok(())
    }
}
```

**预期收益:** 状态空间从 2^5 × 3 = 96 种降到 3 种（无压缩/裁剪/摘要），逻辑清晰，调试简单。

---

## OPT-03: 运行时特性开关导致的代码膨胀

**Claude Code 问题:**
```typescript
// Claude Code 使用 GrowthBook 运行时特性开关
const voiceCommand = feature('VOICE_MODE') ? require('./commands/voice') : null
// 已知开关: PROACTIVE, KAIROS, BRIDGE_MODE, DAEMON, VOICE_MODE,
//          COORDINATOR_MODE, HISTORY_SNIP, CACHED_MICROCOMPACT,
//          CONTEXT_COLLAPSE, TOKEN_BUDGET ...
```
运行时特性开关意味着：
- 所有分支的代码都被打包进最终产物
- 运行时多了 GrowthBook SDK 的网络请求和计算
- 特性开关组合测试困难

**Code Iris 改进:**
```toml
# Cargo.toml — 编译期 feature flags
[features]
default = ["anthropic", "openai-compat"]
anthropic = []
openai-compat = []
google = []
bedrock = ["dep:aws-sdk-bedrockruntime"]
mcp = ["dep:mcp-sdk"]
voice = ["dep:cpal"]  # 完全不编译，零开销
```

**预期收益:** 编译期消除无用代码，二进制体积减小 30-50%，零运行时开销。

---

## OPT-04: 工具执行时序优化

**Claude Code 问题:**
```
Claude Code 的流式工具执行器:
  LLM 流式输出期间 → 检测到 tool_use → 立即开始执行
  但: 工具的 input JSON 可能还没接收完
  所以: 需要 input_json_delta 增量拼接 → 拼完后才能真正执行
  实际效果: 复杂的增量解析逻辑，收益有限
```

Claude Code 试图在 LLM 还在输出时就开始执行工具（"流式工具执行"），但由于 tool_use 的 input JSON 是增量传输的，实际上要等 JSON 拼完才能执行。这个优化带来了巨大的代码复杂度，但实际收益很小（多数工具执行时间远大于等待 JSON 的时间）。

**Code Iris 改进:**
```rust
// 简单直接: 等一个 tool_use block 完整接收后立即执行
// 多个 tool_use 并行执行 (tokio::JoinSet)
let mut join_set = tokio::task::JoinSet::new();

for tool_call in completed_tool_calls {
    let tool = self.tools.get(&tool_call.name)?;
    join_set.spawn(async move {
        tool.execute(tool_call.input).await
    });
}

// 并行收集结果
while let Some(result) = join_set.join_next().await {
    results.push(result??);
}
```

**预期收益:** 代码复杂度降低 80%，并行执行多工具的收益 > 流式单工具的收益。

---

# P1: 性能优化

## OPT-05: 启动时间

**Claude Code 问题:**
```
启动流程 (约 300-500ms):
  1. Bun 加载 JS bundle
  2. GrowthBook 特性开关初始化 (网络请求)
  3. MDM 配置读取
  4. macOS Keychain 预取
  5. API 预连接
  6. React/Ink 渲染器初始化
```

即使 Claude Code 已经做了并行预取优化，Bun 运行时加载 + React/Ink 初始化仍然是不可压缩的开销。

**Code Iris 改进:**
```
启动流程 (目标 <10ms):
  1. 原生二进制，零加载时间               ~0ms
  2. 无运行时特性开关                      ~0ms
  3. 配置文件读取 (本地 TOML)             ~1ms
  4. crossterm 终端初始化                  ~2ms
  5. Ratatui 首帧渲染                     ~3ms
  ────────────────────────────────
  总计                                   <10ms
```

**预期收益:** 启动速度提升 30-50x。

---

## OPT-06: 内存占用

**Claude Code 问题:**
```
基础内存占用:
  Bun 运行时          ~30-50MB
  React/Ink 虚拟 DOM  ~10-20MB
  GrowthBook SDK      ~5MB
  JS 堆 (对话历史)     增长型，GC 回收不及时
  ────────────────────
  空闲状态             ~80-120MB
```

Node/Bun 运行时 + React 虚拟 DOM 是固定底线。GC 回收不及时时对话历史可能膨胀到数百 MB。

**Code Iris 改进:**
```
基础内存占用:
  Rust 二进制          ~2-5MB
  Ratatui 帧缓冲      ~1MB
  对话历史 (Vec)       精确控制，无 GC
  ────────────────────
  空闲状态             ~5-10MB
```

```rust
// 精确内存控制: 对话历史超过阈值时主动压缩
impl AgentLoop {
    fn check_memory(&mut self) {
        let msg_bytes: usize = self.messages.iter()
            .map(|m| m.content.len())
            .sum();

        if msg_bytes > 50 * 1024 * 1024 {  // 50MB
            self.compress(CompressionStrategy::Summarize);
        }
    }
}
```

**预期收益:** 内存占用减少 10-20x，无 GC 停顿。

---

## OPT-07: 文件搜索性能

**Claude Code 问题:**
```
GrepTool 实现: 调用 ripgrep 子进程
  fork() + exec("rg") + pipe stdout + parse output
  每次搜索: ~10-20ms 进程开销
```

每次 Grep/Glob 都 fork 一个子进程，有进程创建和 IPC 开销。

**Code Iris 改进:**
```rust
// 直接使用 grep crate (ripgrep 的库版本)
// 零进程开销，内存中直接返回结果
use grep_regex::RegexMatcher;
use grep_searcher::Searcher;

fn grep_in_process(pattern: &str, path: &Path) -> Result<Vec<Match>> {
    let matcher = RegexMatcher::new(pattern)?;
    let mut matches = Vec::new();
    Searcher::new().search_path(&matcher, path, |line_number, line| {
        matches.push(Match { line_number, content: line.to_string() });
        Ok(true)
    })?;
    Ok(matches)
}
```

**预期收益:** 文件搜索速度提升 2-5x（消除进程开销）。

---

## OPT-08: 流式渲染卡顿

**Claude Code 问题:**
```
React/Ink 渲染管线:
  SSE event → 更新 React state → 虚拟 DOM diff → 终端写入
  问题:
  - React re-render 在长输出时变慢
  - Ink 的 diff 算法不是为高频更新设计的
  - GC 停顿导致偶发卡顿 (用户可感知的 "jank")
```

**Code Iris 改进:**
```rust
// Ratatui immediate-mode: 直接写终端，无虚拟 DOM
// 每个 SSE event → 追加到 buffer → 下一帧渲染
// 渲染时间恒定 O(可见行数)，不随历史长度增长

struct ChatPanel {
    lines: Vec<StyledLine>,       // 所有历史行
    scroll_offset: usize,         // 当前滚动位置
    viewport_height: usize,       // 可见高度
}

impl Widget for &ChatPanel {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // 只渲染可见区域，O(viewport_height)
        let visible = &self.lines[self.scroll_offset..][..self.viewport_height];
        for (i, line) in visible.iter().enumerate() {
            buf.set_line(area.x, area.y + i as u16, line);
        }
    }
}
```

**预期收益:** 渲染延迟从 10-50ms 降到 <1ms，零 GC 卡顿。

---

# P2: 安全优化

## OPT-09: 供应链攻击面

**Claude Code 问题:**
```
依赖链:
  npm install @anthropic-ai/claude-code
  → 下载 500+ npm 包
  → 任何一个包的 postinstall 可执行任意代码
  → 示例: event-stream (2018), ua-parser-js (2021), axios (2025)

风险:
  - 任何依赖的维护者被攻破 → 所有用户受影响
  - postinstall 脚本在安装时就执行，无需用户交互
  - npm audit 漏报率高
```

**Code Iris 改进:**
```
依赖链:
  cargo install code-iris
  → 下载 ~50 Rust crate
  → 无 postinstall 机制 (Rust build.rs 仅编译期)
  → 全部编译为静态二进制

安全工具链:
  cargo audit     — RustSec 漏洞扫描
  cargo vet       — Mozilla 出品，团队共享审计结果
  cargo deny      — 许可证 + 来源 + 重复依赖检查
  cargo supply-chain — 维护者信任链分析
```

**预期收益:** 攻击面减少 90%+。

---

## OPT-10: API Key 存储安全

**Claude Code 问题:**
```
API Key 存储:
  环境变量 / .env 文件 → 明文
  进程内存中 → JavaScript 字符串，GC 回收时机不确定
  可能出现在:
    - core dump
    - 内存交换 (swap)
    - 进程 /proc/pid/environ
    - 错误日志
```

**Code Iris 改进:**
```rust
use secrecy::{ExposeSecret, SecretString, Zeroize};

struct ProviderConfig {
    name: String,
    api_key: SecretString,  // 内存中加密，Drop 时自动清零
}

// 读取 key 只能通过 expose_secret()，防止意外日志泄露
impl LlmProvider for AnthropicProvider {
    async fn chat_stream(&self, ...) -> Result<...> {
        let key = self.api_key.expose_secret();
        // key 离开作用域后自动 Zeroize
    }
}

// Debug/Display trait 不会显示 key 内容
// println!("{:?}", config);  → ProviderConfig { name: "anthropic", api_key: [REDACTED] }
```

**预期收益:** API key 内存安全，杜绝泄露。

---

## OPT-11: 工具执行沙箱

**Claude Code 问题:**
```
BashTool 执行:
  child_process.exec(command)
  → 直接在用户 shell 中执行
  → 权限等同于运行 Claude Code 的用户
  → 唯一防护: 权限提示 (用户可 bypass)

风险:
  - LLM 幻觉生成危险命令 (rm -rf /)
  - prompt injection 导致恶意命令执行
```

**Code Iris 改进:**
```rust
// 分级沙箱策略
enum SandboxLevel {
    /// 完全信任 (用户明确选择)
    None,

    /// 基础防护: 命令黑名单 + 路径限制
    Basic {
        blocked_commands: Vec<String>,  // rm -rf, mkfs, dd, etc.
        allowed_paths: Vec<PathBuf>,    // 仅允许在项目目录内操作
    },

    /// 严格模式 (默认): Basic + 网络限制
    Strict {
        basic: BasicSandbox,
        allow_network: bool,
        max_runtime: Duration,
    },
}

impl BashTool {
    async fn execute(&self, command: &str) -> Result<ToolResult> {
        // 1. 命令预检
        self.sandbox.check_command(command)?;

        // 2. 路径检查
        self.sandbox.check_paths(command)?;

        // 3. 执行 (带超时)
        let output = tokio::time::timeout(
            self.sandbox.max_runtime(),
            self.run_sandboxed(command)
        ).await??;

        Ok(output)
    }
}
```

**预期收益:** 默认安全，防止 LLM 幻觉导致的破坏性命令。

---

# P3: 用户体验优化

## OPT-12: 上下文感知的欢迎界面

**Claude Code 问题:**
```
启动时显示:
  - 固定的 Welcome back!
  - Tips for getting started (固定文案)
  - Recent activity: "No recent activity"
  → 缺乏上下文感知，每次都是相同的静态内容
```

**Code Iris 改进:**
```rust
struct WelcomeContext {
    // 检测项目类型 → 显示针对性提示
    project_type: Option<ProjectType>,  // Rust/Python/Node/Go

    // 检测 git 状态 → 显示有用信息
    git_branch: Option<String>,
    uncommitted_changes: usize,

    // 恢复最近会话
    recent_sessions: Vec<SessionSummary>,

    // 扫描结果缓存
    cached_stats: Option<ProjectStats>,
}

// 动态 Tips:
//   检测到 Python 项目 → "Try `scan .` to analyze your Python architecture"
//   检测到未提交更改 → "3 uncommitted files on branch feature/auth"
//   有最近会话 → "Resume last session? (2h ago, topic: refactor auth)"
```

**预期收益:** 从"打开就有用"提升到"打开就知道上下文"。

---

## OPT-13: 状态栏信息密度

**Claude Code 问题:**
```
底部状态栏:
  git:(main) | Opus 4.6 (200k) | [████░░] 0% | 0s | $0.0000 | 1files

问题:
  - 初始状态大部分信息是 0/空，浪费空间
  - 没有网络状态指示
  - 没有工具执行进度
```

**Code Iris 改进:**
```
空闲状态 (精简):
  main | claude-sonnet-4.6 | ready

对话中 (信息丰富):
  main | claude-sonnet-4.6 | ⟳ streaming... 1.2k tokens | $0.0032

工具执行中:
  main | claude-sonnet-4.6 | ⚡ bash: npm test (3.2s) | 2.1k tokens | $0.0048

错误状态:
  main | claude-sonnet-4.6 | ✗ API timeout, retrying...
```

**预期收益:** 状态栏根据上下文动态调整，信息密度更高。

---

## OPT-14: Token 成本实时可视化

**Claude Code 问题:**
```
Token 追踪:
  - 显示 $0.0000 格式
  - 仅在消息结束后更新
  - 没有预算预警
  - 没有历史趋势
```

**Code Iris 改进:**
```rust
struct CostTracker {
    session_cost: f64,
    turn_cost: f64,
    budget_limit: Option<f64>,
    history: Vec<TurnCost>,  // 每轮的 token/cost 记录
}

// 实时更新:
//   流式 token → 估算成本 → 实时更新状态栏
//   接近预算 80% → 黄色警告
//   接近预算 95% → 红色警告 + 提示
//
// 会话结束时显示摘要:
//   Session: 12 turns | 45.2k tokens | $0.1523
//   Avg: 3.8k tokens/turn | $0.0127/turn
```

**预期收益:** 用户对成本有更清晰的感知和控制。

---

## OPT-15: 命令补全与历史

**Claude Code 问题:**
```
输入框:
  简单的文本输入
  无命令历史 (按 ↑↓ 无效)
  无命令补全
  无多行编辑
```

**Code Iris 改进:**
```rust
struct InputBox {
    // 命令历史 (↑↓ 翻阅)
    history: Vec<String>,
    history_index: Option<usize>,

    // 补全 (Tab 触发)
    completions: Vec<String>,  // scan, arch, deps, stats, model, ...

    // 多行输入 (Shift+Enter 换行)
    multiline: bool,
    lines: Vec<String>,

    // Emacs 快捷键
    // Ctrl+A 行首, Ctrl+E 行尾, Ctrl+K 删除到行尾
    // Ctrl+R 反向搜索历史
}
```

**预期收益:** 交互效率提升，减少重复输入。

---

# P4: 可维护性优化

## OPT-16: 编译期 vs 运行时 保证

**Claude Code 问题:**
```typescript
// TypeScript 的 as/any 逃生舱
const result = response as any;
const toolInput = JSON.parse(deltaStr) as ToolInput;
// 运行时可能 crash

// Zod 运行时校验 — 每次工具调用都要校验
const validated = toolSchema.parse(input);
// 有运行时开销
```

**Code Iris 改进:**
```rust
// serde 编译期 + 运行时双重保证
#[derive(Deserialize)]
struct ToolInput {
    command: String,
    #[serde(default = "default_timeout")]
    timeout: u64,
}

// 反序列化失败 = 编译期类型定义的 Result::Err
// 无需额外的 runtime schema 校验层
let input: ToolInput = serde_json::from_value(raw_input)?;
```

**预期收益:** 消除一整类运行时错误，减少校验层代码。

---

## OPT-17: 错误处理统一

**Claude Code 问题:**
```typescript
// 错误处理风格混杂
try { ... } catch (e: any) { ... }           // 吃掉类型信息
throw new Error("...")                        // 字符串错误
return { type: 'error_during_execution' }     // 结果类型编码
// FallbackTriggeredError → 特殊 error 类触发降级
```

**Code Iris 改进:**
```rust
// 统一的错误类型体系
#[derive(thiserror::Error, Debug)]
pub enum AgentError {
    #[error("LLM API error: {0}")]
    LlmApi(#[from] LlmError),

    #[error("Tool execution failed: {tool} — {message}")]
    ToolExecution { tool: String, message: String },

    #[error("Context window exceeded ({tokens} > {limit})")]
    ContextOverflow { tokens: usize, limit: usize },

    #[error("Budget exceeded: ${spent:.4} > ${limit:.4}")]
    BudgetExceeded { spent: f64, limit: f64 },

    #[error("Max turns reached: {0}")]
    MaxTurns(usize),
}

// 每个错误都有类型、可 match、可 recover
match agent.run(input).await {
    Err(AgentError::ContextOverflow { .. }) => compress_and_retry(),
    Err(AgentError::BudgetExceeded { .. }) => ask_user_to_continue(),
    Err(e) => eprintln!("Error: {e}"),
    Ok(result) => display(result),
}
```

**预期收益:** 错误处理可预测、可恢复、可测试。

---

## OPT-18: 测试友好架构

**Claude Code 问题:**
```
测试挑战:
  - QueryEngine 46K 行，难以单元测试
  - 依赖真实 API 调用 (需要 mock 整个 SDK)
  - React/Ink 组件测试需要 terminal emulator
  - 特性开关组合爆炸
```

**Code Iris 改进:**
```rust
// Provider trait → 轻松 mock
struct MockProvider {
    responses: VecDeque<Vec<StreamEvent>>,
}

#[async_trait]
impl LlmProvider for MockProvider {
    async fn chat_stream(&self, ...) -> Result<...> {
        Ok(stream::iter(self.responses.pop_front().unwrap()))
    }
}

// Tool trait → 轻松 mock
struct MockBashTool {
    expected_commands: Vec<(String, String)>,  // (input, output)
}

// 集成测试: 无需真实 API
#[tokio::test]
async fn test_agent_tool_loop() {
    let provider = MockProvider::with_tool_call("bash", json!({"command": "ls"}));
    let tools = ToolRegistry::with(vec![MockBashTool::new("file.txt\n")]);
    let mut agent = AgentLoop::new(provider, tools, AgentConfig::default());

    let result = agent.run("list files").await.unwrap();
    assert_eq!(result.turns, 2);  // 1 tool call + 1 final response
}
```

**预期收益:** 100% 可离线测试，CI 不依赖外部 API。

---

# 优化矩阵总览

| ID | 优化项 | 类型 | 难度 | 收益 | Phase |
|----|--------|------|------|------|-------|
| OPT-01 | QueryEngine 巨型文件拆分 | 架构 | 低 | 高 | 1 |
| OPT-02 | 压缩管道简化 (5级→2级) | 架构 | 中 | 高 | 2 |
| OPT-03 | 编译期特性开关 | 架构 | 低 | 中 | 1 |
| OPT-04 | 工具并行执行替代流式执行 | 架构 | 中 | 中 | 2 |
| OPT-05 | 启动时间 (300ms→<10ms) | 性能 | 低 | 高 | 1 |
| OPT-06 | 内存占用 (100MB→10MB) | 性能 | 低 | 高 | 1 |
| OPT-07 | 进程内 grep 替代子进程 | 性能 | 中 | 中 | 2 |
| OPT-08 | immediate-mode 渲染 | 性能 | 中 | 高 | 2 |
| OPT-09 | 零 npm 供应链安全 | 安全 | 低 | 极高 | 1 |
| OPT-10 | API key 内存安全 | 安全 | 低 | 高 | 1 |
| OPT-11 | 工具执行沙箱 | 安全 | 高 | 高 | 2 |
| OPT-12 | 上下文感知欢迎界面 | UX | 中 | 中 | 2 |
| OPT-13 | 动态状态栏 | UX | 低 | 中 | 2 |
| OPT-14 | Token 成本可视化 | UX | 低 | 中 | 2 |
| OPT-15 | 命令补全与历史 | UX | 中 | 高 | 2 |
| OPT-16 | 编译期类型保证 | 维护 | 低 | 高 | 1 |
| OPT-17 | 统一错误处理 | 维护 | 低 | 高 | 1 |
| OPT-18 | 测试友好架构 | 维护 | 中 | 极高 | 1 |

### Phase 1 必做 (设计阶段自带):
OPT-01, 03, 05, 06, 09, 10, 16, 17, 18

### Phase 2 关键 (Agent + TUI 阶段):
OPT-02, 04, 07, 08, 11, 12, 13, 14, 15
