# Demo 录制脚本

> 目标：录一个 ~2 分钟的 GIF/视频，展示 code-iris 的三个核心功能。
> 工具推荐：`asciinema rec demo.cast` → `agg demo.cast demo.gif`

---

## 准备工作

```bash
# 安装录制工具
brew install asciinema
cargo install agg   # asciinema → GIF 转换器

# 准备测试项目（用 code-iris 自身最好）
cd ~/project/2026/code-iris
export ANTHROPIC_API_KEY=sk-ant-...
```

终端设置：
- 字体：Fira Code / JetBrains Mono，14px
- 窗口大小：220×50
- 主题：暗色（One Dark / Dracula）

---

## 场景一：TUI 基本使用（30 秒）

```bash
asciinema rec scene1.cast
code-iris
```

在 TUI 里输入：
```
请用 Rust 写一个读取 JSON 文件并打印所有 key 的小程序
```

等 agent 回复（会有语法高亮的代码块），录制结束按 Ctrl+D。

**展示点：** 代码块语法高亮、streaming 动画、工具调用显示

---

## 场景二：iris plan 三步 pipeline（60 秒）

```bash
asciinema rec scene2.cast
iris plan "给这个 Rust 项目加一个 HTTP API，支持查询 git log"
```

等三步跑完（约 30-60 秒），会看到：
```
[1/3] ◌ product      — running…
[1/3] ✓ product      — done
[2/3] ◌ architecture — running…
[2/3] ✓ architecture — done
[3/3] ◌ implementation — running…
[3/3] ✓ implementation — done
```

**展示点：** 三步 pipeline、每步用不同 agent 类型、结果结构化

---

## 场景三：doc-sync 文档漂移检测（20 秒）

```bash
asciinema rec scene3.cast

# 先改一个文件，模拟代码变更
echo "// new feature" >> crates/iris-core/src/agent.rs
git add -A && git commit -m "add feature"

# 运行 doc-sync
iris doc-sync --since HEAD~1
```

**展示点：** 自动检测文档是否过时，给出具体段落建议

---

## 合并 GIF

```bash
# 分别转换
agg scene1.cast scene1.gif
agg scene2.cast scene2.gif
agg scene3.cast scene3.gif

# 或者录一个完整的
asciinema rec full-demo.cast
agg full-demo.cast --cols 180 --rows 45 demo.gif
```

完成后把 `demo.gif` 放到项目根目录，README 里加：

```markdown
![demo](./demo.gif)
```

---

## HN 发帖时间建议

- **周二/周三 上午 9-11 点（美东时间）** = Show HN 最佳时间
- 即北京时间 周二/周三 晚上 9-11 点
