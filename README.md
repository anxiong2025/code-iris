# Code Iris

> See through your code — Rust-powered architecture analyzer & AI agent

AI 编程助手的时代已经到来，但现有工具普遍存在两个问题：**启动慢、吃内存、依赖复杂**，以及**只会聊代码，看不透架构**。

Code Iris 的出发点很简单：用 Rust 重新思考 AI 编程助手应该长什么样。

设计上，我们深入研究了 [Claude Code](https://github.com/anthropics/claude-code) 公开的架构思想 —— tool-call 驱动的 Agent 循环、流式响应、分级权限模型、多 Agent 协作 —— 将这些经过工程验证的模式用 Rust 重新实现，并在此基础上做了三件事：

- **极致轻量**：单二进制分发，冷启动 ~1ms，内存占用 ~15MB，零 npm 依赖
- **架构透视**：不只是聊天，而是真正读懂项目结构、模块依赖、代码演进
- **开放生态**：支持 Anthropic、OpenAI、Google 及 15+ 国内主流提供商，不绑定任何平台

## 为什么选 Rust

| 维度 | Code Iris (Rust) | 传统方案 (Node/Python) |
|------|-----------------|----------------------|
| 启动速度 | ~1ms | ~300-500ms |
| 内存占用 | ~10-20MB | ~50-100MB |
| 分发方式 | 单二进制，零依赖 | 需要运行时环境 |
| 供应链安全 | 零 npm，无 postinstall | npm 生态投毒风险 |
| TLS | rustls (纯 Rust) | 依赖系统 OpenSSL |

## 核心模块

| # | 模块 | 实现文件 | 功能 |
|---|------|---------|------|
| 1 | **Entry Layer** | `iris-cli` · `iris-tui` | CLI 流式输出 + Ratatui 终端界面，slash 命令，异步 Agent Worker |
| 2 | **Agent Loop** | `iris-core/agent.rs` · `context.rs` | tool-call 驱动循环（MAX 20 轮）、4 级上下文压缩、LLM autocompact |
| 3 | **Tools System** | `iris-core/tools/` | 13 个工具：Bash · FileRead/Write/Edit · Grep · Glob · WebFetch · WebSearch · Task×4 · AgentTool · SendMessage |
| 4 | **Commands System** | `iris-cli/main.rs` · `iris-tui/main.rs` | `/help` `/model` `/compact` `/clear` `/session` 及 scan · arch · deps · stats 子命令 |
| 5 | **Permissions System** | `iris-core/permissions.rs` | Default / Plan / Auto / Custom 四种模式，危险工具交互确认，`format_preview()` |
| 6 | **Multi-agent** | `iris-core/coordinator.rs` · `tools/send_message.rs` | Coordinator 并发分发子任务 + synthesis 聚合，MessageBus 跨 Agent 广播通信 |

## 架构

### 总览

```
┌─────────────────────────────────────────────────────────┐
│                      Code Iris                          │
│          Rust-powered AI Agent & Code Analyzer          │
├────────────────────────┬────────────────────────────────┤
│      iris-tui          │         iris-cli               │
│   终端交互界面 (TUI)    │     纯命令行 / 脚本友好         │
└────────────┬───────────┴──────────────┬─────────────────┘
             │                          │
             ▼                          ▼
┌─────────────────────────────────────────────────────────┐
│                     iris-core                           │
│              核心引擎 (UI 无关)                          │
│                                                         │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌────────┐ │
│  │  Agent   │  │  Tools   │  │ Scanner  │  │Context │ │
│  │  Loop    │  │ Registry │  │ (AST分析)│  │ Mgmt   │ │
│  └────┬─────┘  └────┬─────┘  └──────────┘  └────────┘ │
│       │              │                                   │
│  ┌────▼─────┐  ┌─────▼────┐  ┌──────────┐  ┌────────┐ │
│  │Permissions│  │Coordinator│  │ Storage  │  │Config  │ │
│  │  System  │  │(多 Agent) │  │(Sessions)│  │        │ │
│  └──────────┘  └──────────┘  └──────────┘  └────────┘ │
└─────────────────────────┬───────────────────────────────┘
                          │
                          ▼
┌─────────────────────────────────────────────────────────┐
│                     iris-llm                            │
│              LLM 提供商适配层                            │
│                                                         │
│   Anthropic │ OpenAI-compat │ Google │ 15+ 国内提供商   │
└─────────────────────────────────────────────────────────┘
```

### 分层详解

**① 入口层 (iris-tui / iris-cli)**
```
用户输入
  │
  ├─ iris-tui ──▶ tokio task (agent_worker)
  │               mpsc channel ──▶ AgentEvent (TextChunk / ToolCall / Done)
  │               ratatui 渲染 (异步事件循环)
  │
  └─ iris-cli ──▶ clap 命令解析
                  streaming callback (on_text)
                  slash 命令 (/help /model /compact …)
```

**② Agent 循环 (iris-core/agent.rs)**
```
chat(user_input)
  └── loop (max 20 turns)
        ├── [1] 上下文压缩    截断 → 丢弃旧轮次 → 折叠 → LLM 摘要
        ├── [2] callModel()   SSE 流式 → TextDelta / ToolUse
        ├── [3] 权限检查      串行 (Default 交互确认 / Auto 直通)
        ├── [4] 并行工具执行   tokio::JoinSet → 保序追加结果
        └── [5] needsFollowUp? → continue / return
```

**③ 工具系统 (iris-core/tools/)**
```
ToolRegistry
  ├── Bash            Shell 命令执行
  ├── FileRead/Write/Edit   文件操作三件套
  ├── Grep / Glob     代码搜索
  ├── WebFetch        网页抓取 + HTML 转纯文本
  ├── WebSearch       DuckDuckGo (无需 API key)
  ├── Task ×4         任务管理 (Create/Update/List/Get)
  ├── AgentTool       启动子 Agent (minimal_registry 防递归)
  └── SendMessage     Agent 间消息总线 (broadcast channel)
```

**④ 多 Agent 协调 (iris-core/coordinator.rs)**
```
Coordinator::run(tasks)
  ├── spawn sub-agent-0  ──┐
  ├── spawn sub-agent-1  ──┼── tokio::JoinSet 并发
  ├── spawn sub-agent-N  ──┘
  │         ↕ MessageBus (broadcast)
  └── synthesis agent  ←── 汇总所有子结果 → 最终输出
```

### 数据流

```
用户输入 ──▶ Agent Loop ──▶ LLM (流式)
                │                │
                │         TextDelta ──▶ 实时渲染
                │
                └──▶ ToolUse ──▶ 权限检查 ──▶ 并行执行
                                                  │
                              ToolResult ◀─────────┘
                                  │
                              下一轮 LLM 调用
```

详见 [ARCHITECTURE.md](./ARCHITECTURE.md)。

## 安装

```bash
# 从源码构建
git clone https://github.com/anxiong2025/code-iris.git
cd code-iris
cargo build --release

# 安装到系统 (TUI 版)
cargo install --path crates/iris-tui

# 安装到系统 (CLI 版)
cargo install --path crates/iris-cli
```

## 使用

### CLI 模式 (脚本友好)

```bash
# 扫描项目结构
iris scan .

# 生成架构报告
iris arch . --output report.md

# 分析依赖关系
iris deps .

# 查看统计信息
iris stats .

# 配置 API keys
iris configure

# 列出可用模型
iris models
```

### TUI 模式 (交互式)

```bash
# 启动终端界面
code-iris

# 指定提供商和模型
code-iris --provider anthropic --model claude-sonnet-4-6-20250514
```

## 支持的 LLM 提供商

### 国际

| 提供商 | 环境变量 | 默认模型 |
|--------|---------|---------|
| Anthropic (Claude) | `ANTHROPIC_API_KEY` | claude-sonnet-4-6-20250514 |
| OpenAI (GPT) | `OPENAI_API_KEY` | gpt-4o |
| Google (Gemini) | `GOOGLE_API_KEY` | gemini-2.0-flash |
| Groq | `GROQ_API_KEY` | llama-3.3-70b-versatile |
| OpenRouter | `OPENROUTER_API_KEY` | anthropic/claude-sonnet-4 |

### 中国

| 提供商 | 环境变量 | 默认模型 |
|--------|---------|---------|
| DeepSeek | `DEEPSEEK_API_KEY` | deepseek-chat |
| 智谱 AI (GLM) | `ZHIPU_API_KEY` | glm-4-flash |
| 通义千问 (Qwen) | `DASHSCOPE_API_KEY` | qwen-plus |
| 月之暗面 (Kimi) | `MOONSHOT_API_KEY` | moonshot-v1-8k |
| 百川智能 | `BAICHUAN_API_KEY` | Baichuan4-Air |
| MiniMax (稀宇) | `MINIMAX_API_KEY` | MiniMax-Text-01 |
| 零一万物 (Yi) | `YI_API_KEY` | yi-lightning |
| 硅基流动 | `SILICONFLOW_API_KEY` | deepseek-ai/DeepSeek-V3 |
| 阶跃星辰 | `STEPFUN_API_KEY` | step-2-16k |
| 讯飞星火 | `SPARK_API_KEY` | generalv3.5 |
| Ollama (本地) | — | 自动检测 |

## 安全

- **零 npm 依赖** — 完全消除 npm 供应链攻击面
- **rustls-tls** — 纯 Rust TLS 实现，不依赖 OpenSSL
- **cargo audit** — RustSec 已知漏洞扫描
- **cargo vet** — Mozilla 出品的依赖审查系统
- **cargo deny** — 许可证 + 依赖来源检查
- **secrecy** — API key 内存保护，释放时自动清零
- **静态链接** — 单二进制分发，无运行时依赖

## 开发

```bash
# 检查编译
cargo check

# 运行测试
cargo test

# 安全审计
cargo audit
cargo deny check

# 格式化
cargo fmt

# 代码检查
cargo clippy
```

## 路线图

- [x] **Phase 1** — Core + LLM + CLI：多提供商适配、流式输出、代码扫描
- [x] **Phase 2** — Agent Loop + TUI：tool-call 驱动循环、权限系统、4级上下文压缩、多 Agent 协调、ratatui 终端界面
- [ ] **Phase 3** — 深度代码理解：多语言 AST 分析、依赖图可视化、MCP 协议集成
- [ ] **Phase 4** — 桌面端：Tauri GUI，跨平台原生体验

## 致谢

本项目的 Agent 架构设计深受 [Claude Code](https://github.com/anthropics/claude-code)（Anthropic）公开架构思想的启发，特此致谢。Code Iris 是基于这些公开设计理念的独立 Rust 实现，与 Anthropic 官方无任何关联。

## 协议与免责声明

本项目采用 [MIT License](./LICENSE) 开源。

> **免责声明**
>
> - 本项目仅供**个人学习、技术研究与非商业用途**，不得用于任何商业目的
> - 项目中涉及的架构设计思想参考自 Claude Code 等公开资料，相关知识产权归其原始权利人所有
> - 使用本软件所产生的任何法律责任由使用者自行承担，作者不承担连带责任
> - 本项目与 Anthropic 公司无任何隶属、授权或合作关系
