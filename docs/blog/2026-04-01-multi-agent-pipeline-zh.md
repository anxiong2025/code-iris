# 我用 Rust 写了一个 AI 编程 Agent，核心是一个类型化的多 Agent Pipeline

**GitHub：** https://github.com/anxiong2025/code-iris

---

## 为什么要做这个？

市面上的 AI 编程工具大多是套壳——把 LLM 接上 Shell，加个文件读写，就说自己是 Agent。我想搞清楚从头构建一个 Agent 真正需要解决什么问题，同时有一个具体的架构问题想探索：**当多个 Agent 需要协作时，结构化的上下文传递到底该怎么做？**

于是有了 code-iris，一个用 Rust 写的终端 AI 编程 Agent。它有 14 个工具、基于 ratatui 的 TUI 界面、多 LLM 提供商支持，以及这篇文章要重点说的——一个类型化的多 Agent Pipeline。

---

## 核心问题：多 Agent 之间怎么交接上下文？

把多个 Agent 串联起来工作时，下一个 Agent 需要知道上一个做了什么、得出了什么结论。

**方案一：塞进对话历史。** 把所有上下文丢进同一条消息链，让下一个 Agent 自己读。问题很明显：上下文窗口会撑满，Agent 的工具调用过程和最终结论混在一起难以区分，没有办法结构化地提取"Planner 究竟决定了什么"。

**方案二：文件约定。** Agent 1 写 `plan.md`，Agent 2 读取。看起来简单，但没有 Schema，格式任意，拼接起来很脆，也不好做编程层面的组合。

我最终选择的方案：**把已完成步骤的结果直接注入下一个 Agent 的 system prompt，而不是对话历史。** 每个完成的步骤以带标签的结构化块呈现给下一个 Agent，作为背景知识存在，而不是一个需要回应的对话轮次。

---

## 设计：PipelineStep 和结构化 context 注入

核心类型很简洁：

```rust
pub struct PipelineStep {
    pub label: String,
    pub agent_type: Option<String>,  // "explorer" | "worker" | "reviewer" | 自定义
    pub system_prompt: String,
    pub prompt: String,
}
```

`label` 是关键。第 N 步执行完成后，它的输出以 `label` 为 key 存储。下一步的 system prompt 在发送给 LLM 之前会自动注入所有前序结果：

```
# Prior step results

## product
<第一步的结构化输出>

## architecture
<第二步的结构化输出>
```

`pipeline_run()` 自动处理这个累积过程——每一步运行前，所有前序步骤的结果都已经在它的 system context 里。

三种内置 Agent 类型对应不同的能力范围：

- `explorer` — 只读工具权限，使用 Haiku 模型，适合快速分析代码库
- `reviewer` — 只读工具权限，使用主模型，适合做架构设计和评审
- `worker` — 完整工具权限，可以写文件、执行命令

这种能力分层不只是安全考量，也是质量考量：让一个只负责分析的 Agent 拿到写权限没有意义，反而会引入不确定性。

---

## 演示：`iris plan` 跑起来是什么样子

```
$ iris plan "增加 JWT 用户认证"
```

这条命令触发一个三步 Pipeline：

**第一步：explorer 分析现有代码库。** 扫描项目结构，找到已有的鉴权相关代码，检查依赖，记录关键文件路径。全程只读，速度快。

**第二步：reviewer 设计方案。** Explorer 的完整分析在它的 system context 里。它基于这些信息决定：要改哪些文件、JWT 流程怎么设计、有哪些边界情况需要处理。

**第三步：worker 执行实现。** Explorer 的分析和 reviewer 的设计都以结构化块的形式在它的 system context 里。它不需要重新分析代码，直接执行——写代码、改文件、跑测试。

每一步的输出实时流式显示在 TUI 里。执行完成后，不只有代码，还有完整的决策链路。

Pipeline 本身没什么神秘的——就是遍历 `Vec<PipelineStep>` 时做上下文累积。但类型化的表达让它可以组合：可以用这三种内置类型，也可以定义任意数量的自定义步骤。

---

## 其他值得说的功能

**LSP 工具。** 大多数编程 Agent 把代码库当纯文本处理。code-iris 有一个 `lsp` 工具，通过 JSON-RPC stdio 和运行中的 Language Server 通信，支持 hover、go-to-definition、find-references、diagnostics。Agent 可以直接问"这个函数返回什么类型"并拿到准确答案，而不是靠字符串匹配猜测。在重构任务里这个差距很明显。

**doc-sync。** `iris doc-sync --since HEAD~1` 对比最近一次提交的 diff，检查 `.md` 文件中是否有引用了已修改代码的部分。函数被重命名了但文档还用旧名字？直接标出来。文档和代码的同步是个容易被忽略但真实存在的痛点。

**14 个工具。** bash、file_read、file_write、file_edit、grep、glob、lsp、web_fetch、web_search，以及四个任务管理工具（create、update、list、complete），还有 agent_tool 和 send_message 用于启动子 Agent。

**多提供商支持。** Anthropic Claude、任意 OpenAI 兼容接口（OpenRouter、Together、Groq 等 17+ 提供商）、Google Gemini。从环境变量自动检测：设置了 `ANTHROPIC_API_KEY` 用 Anthropic，设置了 `OPENAI_API_KEY` 用 OpenAI，不需要额外配置文件。

---

## 下一步计划

目前还缺的东西：跨会话的持久化 Agent 记忆、流式工具结果展示（现在是等工具执行完才显示输出）、Pipeline 的 DSL 定义格式（现在需要改 Rust 代码才能自定义 Pipeline）、以及 eval 基准测试。

接下来优先做：持久化任务状态、Pipeline 定义格式、更好的步骤输出容错处理，以及 eval 来跟踪迭代质量。

核心设计——类型化步骤、结构化上下文注入、能力分级——这个方向是对的。剩下的是工程迭代。

---

GitHub：https://github.com/anxiong2025/code-iris
