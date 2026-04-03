# 更新日志

code-iris 的所有重要变更记录在此文件中。

格式遵循 [Keep a Changelog](https://keepachangelog.com/)。

## [0.4.1] — 2026-04-03

### 改进
- **Token 用量优化** — `max_tool_result_tokens` 从 8k 降至 4k，`keep_recent_turns` 从 6 降至 4，autocompact 阈值从 80% 提前到 60%
- **旧工具结果驱逐 (L1.5)** — 每轮主动将超过 N 轮的旧 tool_result 替换为一行摘要，避免历史堆积
- **Head+Tail 截断** — 工具结果截断时保留头部 75% + 尾部 25%，不丢失末尾的错误信息或关键输出

## [0.4.0] — 2026-04-03

### 新增
- **Diff 预览** — `file_edit` 和 `file_write` 执行后在 TUI 展示红绿着色的 unified diff，看清每一行改动
- **Extended Thinking 展示** — Claude 模型的 thinking 推理过程以折叠的 💭 形式显示在 TUI 中
- **`/init` 项目扫描** — 自动检测语言/框架/构建命令/目录结构，生成 `.iris/instructions.md` 项目画像
- **Skills 系统** — 内置 `/review`（代码审查）、`/doc`（文档生成）技能，支持自定义 `.iris/skills/*.md` 模板
- **MCP 独立工具注册** — 每个 MCP server 的 tool 展开为独立注册（`mcp__server__tool`），LLM 可精准调用带独立 schema
- **项目级 MCP 配置** — 支持 `.iris/mcp.toml`，项目级覆盖全局同名 server
- **Streaming Tool Call** — 工具名在 SSE 流中立即显示（`ToolUseStart` 事件），不再等待完整 JSON
- **三层记忆系统** — 全局 `~/.code-iris/instructions.md` → 项目 `.iris/instructions.md` → 目录 `.iris/instructions_local.md`
- **Permission 精细化** — 支持 `.iris/permissions.toml` 按 tool 和路径 glob 配置 allow/confirm/deny 规则
- **`/skills` 命令** — 列出所有可用技能（内置 + 自定义）
- **`/memory` 增强** — 显示三层指令文件的加载状态

### 改进
- **Tool 执行结果回显** — `on_tool_result` 回调让 TUI 和 CLI 展示工具执行结果预览
- **MCP 启动时发现** — 替代旧的懒加载单 wrapper 模式，启动时 `tools/list` 获取所有工具定义

## [0.3.1] — 2026-04-03

### 改进
- **web_fetch 重构** — Jina Reader (`r.jina.ai`) 做主抓取，返回干净 Markdown，支持 JS 渲染页面，raw reqwest 作为 fallback
- **web_fetch prompt 参数** — 可指定要从页面提取的内容，大页面按 Markdown 标题分段 + 关键词匹配智能提取相关章节
- **web_fetch 15 分钟缓存** — 同 URL 不重复抓取，自动清理过期条目

## [0.3.0] — 2026-04-03

### 新增
- **AWS Bedrock 完整支持** — InvokeModel API (Anthropic Messages 格式)，Bearer token + SigV4 双认证，自动读取 `~/.aws/credentials` 和 `~/.aws/config`
- **`/model` 自动切换 Provider** — 选择不同 provider 的模型时自动切换后端（如从 qwen 切到 bedrock），支持 fallback 链（anthropic → bedrock）
- **Bedrock 模型名映射** — 短名 `claude-opus-4-6` 自动映射为 Bedrock ID，优先读取 `ANTHROPIC_DEFAULT_OPUS_MODEL` 等环境变量
- **Bedrock Tool Calling** — 完整支持 Anthropic 格式的 tool 定义和 tool_use 响应解析
- **Google Gemini Tool Calling** — 支持 `function_declarations` 格式发送 tool 定义，解析 `functionCall` 响应

### 修复
- **Bedrock tool_use ID 清洗** — 跨 provider 切换时，自动清洗不符合 Anthropic ID 格式的 tool ID
- **LLM 错误信息透传** — 不再只显示 "LLM stream failed"，现在展示完整的 provider 错误详情
- **`/model` 切换失败不污染状态** — 凭证缺失时模型名保持不变，不会设成无效值

## [0.2.0] — 2026-04-01

### 新增
- **`/buddy` 宠物系统** — 18 种 ASCII 宠物，5 档稀有度（普通 60% → 传说 1%），抽到后显示在状态栏
- **`/` 命令补全菜单** — 输入 `/` 自动弹出命令列表 + 描述，Up/Down 选择，Tab/Enter 确认
- **`/model` 模型名补全** — 输入 `/model ` 弹出已知模型列表，避免拼写错误
- **Delete 键** — 光标前向删除
- **Ctrl+U / Ctrl+K** — 删到行首 / 行尾（readline 兼容）
- **Ctrl+Left/Right, Alt+Left/Right** — 按词跳转光标
- **Home / End 键** — 跳转到输入框首尾
- **粘贴支持** — Bracketed Paste，多行文本粘贴不会误触快捷键
- **鼠标滚轮** — 滚轮翻页聊天历史
- **CJK 宽字符支持** — 引入 `unicode-width`，中文光标对齐正确

### 修复
- **Windows 重复输入** — 过滤 crossterm 的 `KeyEventKind::Release` 事件
- **状态栏硬编码 claude** — 现在显示实际检测到的 provider 模型名
- **Bedrock 优先级** — 移到最低优先级兜底，不再抢占其他已配置的 provider
- **Coordinator 硬编码模型** — `pipeline_run` 现在使用检测到的 provider 模型
- **错误信息** — 从 JSON API 错误中提取可读的 message，不再显示原始 JSON

## [0.1.0] — 2026-03-28

### 新增
- Hooks 钩子系统、持久化 Bash 会话、自动压缩上下文
- TUI 语法高亮、输入历史、光标导航
- LSP 工具、TUI Pipeline 视图、`/plan` 命令
- `iris plan`、`iris doc-sync`、TUI `/agents` 命令
- CoordinatorConfig、Agent 类型定义、`pipeline_run()`
- Gemini provider、tree-sitter、任务持久化、斜杠命令
- TUI 模型切换、compact、`iris login/logout`、MCP 配置
- 多 Provider 支持 — 自动检测已配置的 API Key
- 重试逻辑、Claude OAuth、MCP 客户端
- AWS Bedrock provider（基础版）
