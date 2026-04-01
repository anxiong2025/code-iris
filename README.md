# Code Iris

> Rust-powered AI coding agent — single binary, 17+ LLM providers, typed multi-agent pipeline

[![Crates.io](https://img.shields.io/crates/v/iris-cli.svg)](https://crates.io/crates/iris-cli)
[![CI](https://github.com/anxiong2025/code-iris/actions/workflows/ci.yml/badge.svg)](https://github.com/anxiong2025/code-iris/actions)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](./LICENSE)

## 30 秒上手

```bash
# 安装（需要 Rust 1.85+）
cargo install iris-cli          # CLI
cargo install code-iris         # TUI（Claude Code 风格终端界面）

# 配置 API key（任选其一）
export ANTHROPIC_API_KEY=sk-ant-...   # Claude（推荐）
export DEEPSEEK_API_KEY=sk-...        # DeepSeek（便宜）
export DASHSCOPE_API_KEY=sk-...       # 通义千问（国内）

# 开始
code-iris                             # 打开 TUI
iris chat                             # CLI 交互模式
iris plan "给 REST API 加 JWT 认证"   # 三步自动规划
iris doc-sync --since HEAD~1          # 检测文档与代码是否一致
```

---

## 为什么选 Code Iris

AI 编程工具要么太重（Node.js/Python 运行时），要么功能单薄（只有单 Agent 聊天）。Code Iris 用 Rust 重建，有两个独有能力：

### ① 串行 Pipeline Multi-Agent

```
iris plan "需求"
  → ProductAgent (explorer)  — 分析需求、约束、用户
        ↓ 结构化输出注入下一步 system context
  → ArchAgent   (reviewer)   — 组件设计、接口、风险
        ↓ 结构化输出注入下一步 system context
  → CodeAgent   (worker)     — 生成可用代码 + 测试
```

每一步的输出作为下一步的 system context **结构化注入**，不是自由文本拼接。Claude Code、gstack、LangChain 都没有这个。

### ② 文档漂移检测

```bash
iris doc-sync --since HEAD~1
# → 自动 diff 代码变更 → 扫描 .md 文件 → 找出过时段落
```

### ③ Hooks 系统（类 Claude Code）

```toml
# .iris/hooks.toml
[[hooks]]
event = "PreToolUse"
matcher = "bash"
command = "jq -e '.tool_input.command | test(\"rm -rf\") | not' > /dev/null"

[[hooks]]
event = "PostToolUse"
matcher = "file_write"
command = "iris doc-sync --since HEAD~1"
```

### ④ 分层 Instructions（类 CLAUDE.md）

```markdown
# .iris/instructions.md
你在 code-iris Rust workspace 工作。
- 改 .rs 文件后必须 cargo check
- 错误处理用 anyhow::Result
```

---

## 对比

| | Code Iris | Claude Code | gstack | claw-code |
|---|---|---|---|---|
| 语言 | **Rust** | TypeScript | Shell | Python |
| 分发 | **单二进制** | npm 包 | git clone | pip |
| 启动时间 | **~1ms** | ~300ms | — | — |
| 内存 | **~15MB** | ~100MB+ | — | — |
| 串行 Pipeline | **✅** | ❌ | ❌ | ❌ |
| 文档漂移检测 | **✅** | ❌ | ❌ | ❌ |
| Hooks 系统 | **✅** | ✅ | ❌ | ❌ |
| 分层 Instructions | **✅** | ✅ CLAUDE.md | ❌ | ❌ |
| LSP 集成 | **✅** | ❌ | ❌ | ❌ |
| 持久 bash session | **✅** | ✅ | ❌ | ❌ |
| 多 Provider | **17+** | Anthropic only | Anthropic only | 2 |
| Claude OAuth | **✅** | ✅ | — | — |
| MCP Client | **✅** | ✅ | — | — |
| 开源 | **✅** | ❌ | ❌ | ❌ |

---

## TUI — Claude Code 风格终端界面

```bash
code-iris
```

- Markdown 渲染 + **语法高亮**（syntect，base16-ocean 主题）
- 光标移动（Left/Right/Ctrl+A/E）、历史记录（Up/Down）
- Shift+Enter 多行输入
- Ctrl+C 中断当前 turn，Ctrl+D 退出
- 实时 spinner + 工具调用显示
- `/plan <需求>` `/agents` `/commit` `/memory` `/cd` `/worktree`

---

## 核心功能

### `iris chat` / `code-iris`

```bash
iris chat                              # 自动检测 API key
iris chat --model claude-opus-4-6      # 指定模型
iris chat --plan                       # 只读 Plan Mode
iris chat --resume <session-id>        # 续上次会话
```

### `iris plan` — 三步自动规划

```bash
iris plan "实现用户登录模块"
iris plan "重构数据库层" --arch-only    # 只输出架构，不生成代码
```

### `iris doc-sync` — 文档漂移检测

```bash
iris doc-sync                          # 检测 HEAD~1 以来
iris doc-sync --since HEAD~5
iris doc-sync --since v1.0.0
```

### `iris run` — 多 Agent 任务

```bash
# 并行（fan-out + synthesis）
iris run "分析这个 PR" \
  --sub "security@reviewer:找安全漏洞" \
  --sub "quality@reviewer:评估代码质量" \
  --sub "tests@explorer:检查测试覆盖率"

# 串行 Pipeline
iris run "重构认证模块" --pipeline \
  --sub "explore@explorer:分析现有代码" \
  --sub "review@reviewer:找设计缺陷" \
  --sub "implement@worker:执行重构"
```

---

## Agent 类型系统

| 类型 | 权限 | 默认模型 | 用途 |
|------|------|---------|------|
| `explorer` | 只读 | claude-haiku | 代码探索、结构分析 |
| `reviewer` | 只读 | 主模型 | 代码审查、风险评估 |
| `worker` | 全权限 | 主模型 | 实现、文件修改 |

**自定义 agent**（`.iris/agents/my-agent.toml`）：

```toml
name = "security-auditor"
description = "专注安全漏洞的审查 agent"
sandbox_mode = "read-only"
model = "claude-opus-4-6"
instructions = """
你是安全审计专家，专注 OWASP Top 10、注入攻击、权限提升。
"""
```

---

## 工具列表（14 个）

| 工具 | 说明 |
|------|------|
| `bash` | 持久 bash session（cd/export 跨调用生效） |
| `file_read` / `file_write` / `file_edit` | 文件 I/O |
| `grep` / `glob` | 代码搜索 |
| `lsp` | LSP 代码智能（hover/definition/references/diagnostics） |
| `web_fetch` / `web_search` | 网络访问 |
| `task_create/update/list/get` | 任务管理 |
| `agent_tool` | 召唤子 agent |
| `send_message` | Agent 间通信 |

---

## 架构

```
┌──────────────────────────────────────────────────────┐
│                    Code Iris                         │
├──────────────────────┬───────────────────────────────┤
│     code-iris (TUI)  │        iris (CLI)             │
│   Ratatui 终端界面    │   CLI / CI 脚本友好            │
└───────────┬──────────┴──────────────┬────────────────┘
            ▼                         ▼
┌──────────────────────────────────────────────────────┐
│                    iris-core  (SDK)                  │
│  Agent Loop  │  14 Tools  │  Coordinator  │ AgentDef │
│  Hooks       │  Instructions│  Pipeline   │ Permissions│
│  Context compression (80% autocompact)               │
└───────────────────────────┬──────────────────────────┘
                            ▼
┌──────────────────────────────────────────────────────┐
│                    iris-llm                          │
│  Anthropic │ OpenAI-compat │ Google │ 17+ providers  │
│  OAuth  │  SSE streaming  │  Retry  │  MCP Client    │
└──────────────────────────────────────────────────────┘
```

---

## 安装

```bash
# crates.io（推荐）
cargo install iris-cli      # iris 命令
cargo install code-iris     # code-iris TUI

# 从源码
git clone https://github.com/anxiong2025/code-iris
cd code-iris
cargo install --path crates/iris-cli
cargo install --path crates/iris-tui
```

**系统要求：** Rust 1.85+，无其他运行时依赖。

---

## 支持的 LLM 提供商

| 提供商 | 环境变量 | 默认模型 |
|--------|---------|---------|
| Anthropic | `ANTHROPIC_API_KEY` | claude-sonnet-4-6 |
| OpenAI | `OPENAI_API_KEY` | gpt-4o |
| Google Gemini | `GOOGLE_API_KEY` | gemini-2.0-flash |
| DeepSeek | `DEEPSEEK_API_KEY` | deepseek-chat |
| 通义千问 | `DASHSCOPE_API_KEY` | qwen-plus |
| 月之暗面 | `MOONSHOT_API_KEY` | moonshot-v1-8k |
| 智谱 AI | `ZHIPU_API_KEY` | glm-4-flash |
| 硅基流动 | `SILICONFLOW_API_KEY` | deepseek-ai/DeepSeek-V3 |
| Groq | `GROQ_API_KEY` | llama-3.3-70b-versatile |
| OpenRouter | `OPENROUTER_API_KEY` | anthropic/claude-sonnet-4 |

自动检测：OAuth → `ANTHROPIC_API_KEY` → 其他环境变量（先检测到哪个用哪个）。

---

## 安全

- **零 npm 依赖** — 无供应链攻击面
- **rustls-tls** — 纯 Rust TLS，不依赖 OpenSSL
- **secrecy** — API key 内存保护
- **静态链接** — 单二进制，无运行时
- **权限分级** — Default / Plan / Auto / Custom
- **max_depth = 1** — 防止 agent 递归爆炸
- **max_threads = 6** — 并发上限

---

## 开发

```bash
cargo check --workspace
cargo test --workspace        # 90 个测试
cargo fmt
cargo clippy
```

---

## 路线图

- [x] Phase 1 — 多提供商、流式输出、代码扫描、OAuth
- [x] Phase 2 — Agent 循环、14 工具、权限模型、TUI、多 Agent
- [x] Phase 3a — Pipeline Multi-Agent、Agent 类型系统、doc-sync
- [x] Phase 3b — LSP Tool、持久 bash session、Hooks、Instructions、TUI 语法高亮
- [ ] Phase 4 — crates.io 发布、SDK 文档、Show HN

---

## 文档

- [架构设计](docs/architecture.md) / [中文](docs/architecture-zh.md)
- [Pipeline Handoff 协议](docs/handoff-protocol.md) / [中文](docs/handoff-protocol-zh.md)
- [技术博客（英文）](docs/blog/2026-04-01-multi-agent-pipeline.md)
- [技术博客（中文）](docs/blog/2026-04-01-multi-agent-pipeline-zh.md)

---

## 致谢

Code Iris 从零原创构建。Agent 架构设计参考 Claude Code（Anthropic）公开的设计理念，与 Anthropic 官方无任何关联，不含任何非公开代码。

## 许可证

[MIT](./LICENSE)
