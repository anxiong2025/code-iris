# 更新日志

code-iris 的所有重要变更记录在此文件中。

格式遵循 [Keep a Changelog](https://keepachangelog.com/)。

## [未发布]

### 新增
- **AWS Bedrock 完整支持** — InvokeModel API (Anthropic Messages 格式)，Bearer token + SigV4 双认证，自动读取 `~/.aws/credentials` 和 `~/.aws/config`
- **`/model` 自动切换 Provider** — 选择不同 provider 的模型时自动切换后端（如从 qwen 切到 bedrock），支持 fallback 链（anthropic → bedrock）
- **Bedrock 模型名映射** — 短名 `claude-opus-4-6` 自动映射为 Bedrock ID，优先读取 `ANTHROPIC_DEFAULT_OPUS_MODEL` 等环境变量
- **Bedrock Tool Calling** — 完整支持 Anthropic 格式的 tool 定义和 tool_use 响应解析
- **Google Gemini Tool Calling** — 支持 `function_declarations` 格式发送 tool 定义，解析 `functionCall` 响应
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
- **Bedrock tool_use ID 清洗** — 跨 provider 切换时，自动清洗不符合 Anthropic ID 格式的 tool ID (`^[a-zA-Z0-9_-]+$`)
- **LLM 错误信息透传** — 不再只显示 "LLM stream failed"，现在展示完整的 provider 错误详情
- **`/model` 切换失败不污染状态** — 凭证缺失时模型名保持不变，不会设成无效值
- **Windows 重复输入** — 过滤 crossterm 的 `KeyEventKind::Release` 事件，修复 Windows 下每个按键产生两个字符
- **状态栏硬编码 claude** — 现在显示实际检测到的 provider 模型名
- **Bedrock 优先级** — 移到最低优先级兜底，不再抢占其他已配置的 provider
- **Coordinator 硬编码模型** — `pipeline_run` 现在使用检测到的 provider 模型
- **错误信息** — 从 JSON API 错误中提取可读的 message，不再显示原始 JSON
- **Pipeline 步骤图标** — 移除乱码 emoji，使用 ASCII 标记

## [0.1.0] — 2026-03-28

### 新增
- Hooks 钩子系统、持久化 Bash 会话、自动压缩上下文 (`f8aa3a9`)
- TUI 语法高亮、输入历史、光标导航 (`e643c05`)
- LSP 工具、TUI Pipeline 视图、`/plan` 命令 (`f137648`)
- `iris plan`、`iris doc-sync`、TUI `/agents` 命令 (`09f0ea0`)
- CoordinatorConfig、Agent 类型定义、`pipeline_run()` (`755789a`)
- Gemini provider、tree-sitter、任务持久化、斜杠命令 (`bef09c1`)
- TUI 模型切换、compact、`iris login/logout`、MCP 配置 (`5ea3a53`)
- 多 Provider 支持 — 自动检测已配置的 API Key (`7347b5a`)
- 重试逻辑、Claude OAuth、MCP 客户端 (`0161452`)
- AWS Bedrock provider（进行中）
