#!/bin/bash
claude --dangerously-skip-permissions \
  --max-budget-usd 30.00 \
  --add-dir /Users/robert/project/2026/claude-code/src \
  "你是 Rust 专家，负责完成 code-iris Phase 1 的全部开发。

参考资料：
- Python 原版代码在 /Users/robert/project/2026/claude-code/src/（直接阅读参考）
- 架构设计文档：ARCHITECTURE.md（当前目录）
- 优化目标：OPTIMIZATIONS.md（当前目录）

工作目录：/Users/robert/project/2026/code-iris

按顺序完成以下模块，每完成一个 cargo check 验证，有错误立即修复再继续：

1. crates/iris-llm/src/types.rs
定义核心类型：Role enum (User/Assistant/Tool)，ContentBlock enum (Text/ToolUse/ToolResult)，Message { role, content: Vec<ContentBlock> } + 构造方法 user/assistant/tool_result，StreamEvent enum (TextDelta/ThinkingDelta/ToolUse/Usage/MessageStop)，TokenUsage { input_tokens, output_tokens } + accumulate()，ModelConfig { model, max_tokens, system_prompt, temperature }，ToolDefinition { name, description, input_schema: Value }。全部 derive Serialize, Deserialize, Debug, Clone。

2. crates/iris-llm/src/sse.rs
parse_anthropic_sse(response: reqwest::Response) -> impl Stream<Item = Result<StreamEvent>>
处理：message_start(Usage), content_block_start(tool_use记录id/name), content_block_delta(text_delta→TextDelta, input_json_delta→累积JSON), content_block_stop(tool_use→ToolUse完整input), message_delta(usage), message_stop(MessageStop)。
用 eventsource-stream + async-stream。

3. crates/iris-llm/src/anthropic.rs
AnthropicProvider { api_key: secrecy::SecretString, client: reqwest::Client, base_url: String }
pub fn new(api_key: impl Into<String>) -> Self，client 必须用 rustls-tls。
pub async fn chat_stream(&self, messages: &[Message], tools: &[ToolDefinition], config: &ModelConfig) -> Result<impl Stream<Item = Result<StreamEvent>>>
POST {base_url}/v1/messages，headers: x-api-key/anthropic-version: 2023-06-01/content-type，body: {model, max_tokens, system?, messages, tools?, stream: true}。

4. crates/iris-llm/src/openai.rs
OpenAiCompatProvider { name, api_key: secrecy::SecretString, client, base_url, default_model }
pub async fn chat_stream() POST {base_url}/chat/completions stream:true，解析 data:{choices:[{delta:{content?,tool_calls?}}]}。
ProviderInfo { name, env_key, base_url, default_model, label } 结构体。
PROVIDERS: &[ProviderInfo] 常量，移植 providers.py 全部15个提供商：anthropic/openai/google/groq/openrouter/deepseek/zhipu/qwen/moonshot/baichuan/minimax/yi/siliconflow/stepfun/spark。
pub fn detect_provider() -> Option<&'static ProviderInfo>
pub fn get_provider(name: &str) -> Option<&'static ProviderInfo>

5. crates/iris-llm/src/lib.rs
更新 pub mod 声明（types/sse/anthropic/openai/google），pub use types::*，导出所有公共类型和函数。

6. crates/iris-core/src/models.rs
移植 models.py：Language enum (Python/Rust/TypeScript/JavaScript/Go/Unknown) + from_extension(ext)，ImportType enum (Absolute/Relative/ThirdParty)，Module { name, path, file_count, description }，Dependency { source, target, import_type }，ProjectStats { total_files, total_lines, total_modules, avg_lines_per_file: f64 }，ProjectManifest { root: PathBuf, modules: Vec<Module>, stats: ProjectStats, language: Language } + to_markdown() -> String，ArchReport { title, sections: Vec<String> } + render() -> String。

7. crates/iris-core/src/config.rs
移植 config.py，路径改为 ~/.code-iris/.env：
pub fn load_env() -> HashMap<String,String>（加载并写入 os::environ）
pub fn save_env(env: &HashMap<String,String>) -> Result<PathBuf>
pub fn configure_interactive()（交互式配置向导，参考 providers.py 的 PROVIDERS 列表）
pub fn get_default_provider_name() -> Option<String>

8. crates/iris-core/src/scanner.rs
移植 scanner.py，tree-sitter 替代 Python ast：
pub fn scan_project(root: &Path) -> Result<ProjectManifest>（遍历 .py/.rs/.ts/.js，跳过 __pycache__/.git/target/node_modules/.venv，统计 modules 和 stats）
pub fn scan_dependencies(root: &Path) -> Result<Vec<Dependency>>（tree-sitter-python 解析 import/from import，区分 relative 和 absolute）
更新 iris-core/src/lib.rs 的 pub mod 声明。

9. crates/iris-core/src/reporter.rs
移植 reporter.py：
pub struct Reporter { manifest: ProjectManifest, dependencies: Vec<Dependency> }
pub fn from_path(path: &Path) -> Result<Self>
pub fn render_manifest(&self) -> String
pub fn render_dependencies(&self) -> String
pub fn render_stats(&self) -> String
pub fn render_full_report(&self) -> String
pub fn render_dependency_graph(&self) -> String（新增：Mermaid 格式 graph TD）
更新 iris-core/src/lib.rs。

10. crates/iris-cli/src/main.rs
clap derive CLI，移植 code-robin 全部命令：
scan [path]（默认当前目录，输出 manifest markdown）
arch [path] [-o output.md]（输出完整报告）
deps [path]（输出依赖分析）
stats [path]（输出统计表格）
configure（交互式配置 API keys）
models（列出全部15个提供商，显示 ✓ 已配置 / ✗ 未配置）

验收标准（全部通过才算完成）：
1. cargo check --workspace 零错误零警告
2. cargo build -p iris-cli --release 成功
3. 找到编译好的 iris binary 路径并运行 iris scan . 输出 manifest
4. 运行 iris models 列出全部15个提供商

注意事项：
- 所有依赖用 workspace 定义，子 crate 只写 dep_name.workspace = true
- API key 必须用 secrecy::SecretString 保护
- 所有错误用 anyhow::Result
- 不写无用 import，避免 dead_code 警告
- 不要改动 crates/iris-tui/ 的任何文件"
