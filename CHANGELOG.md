# Changelog

All notable changes to code-iris are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/).

## [Unreleased]

### Fixed
- **TUI: Windows duplicate input** — filter `KeyEventKind::Release` events from crossterm, fixing double characters on Windows (especially CJK input)
- **TUI: CJK wide-char cursor alignment** — use `unicode-width` for correct cursor rendering over 2-cell-wide characters
- **Status bar hardcoded model** — now shows actual detected provider model instead of always `claude-sonnet-4.6`
- **Bedrock provider priority** — moved Bedrock to last-resort fallback, no longer overrides other configured providers
- **Coordinator hardcoded model** — `pipeline_run` now uses detected provider model
- **Error messages** — extract human-readable message from JSON API errors instead of showing raw response
- **Pipeline step icons** — removed broken emoji, use clean ASCII markers

### Added
- **TUI: `/` command Tab completion** — type `/` then Tab to autocomplete; multiple matches show candidate list
- **TUI: Slash command completion menu** — type `/` to see popup with all commands and descriptions, navigate with Up/Down, confirm with Tab/Enter
- **TUI: `/model` model completion** — type `/model ` to see known model names, auto-complete to avoid typos
- **TUI: Delete key** — forward-delete at cursor
- **TUI: Ctrl+U / Ctrl+K** — kill to start / end of line (readline-compatible)
- **TUI: Ctrl+Left/Right, Alt+Left/Right** — word-wise cursor movement
- **TUI: Home / End keys** — jump to start / end of input
- **TUI: Bracketed paste** — paste multi-line text without triggering keybindings
- **TUI: Mouse scroll wheel** — scroll chat history with mouse

## [0.1.0] — 2026-03-28

### Added
- Hooks system, persistent bash session, autocompact (`f8aa3a9`)
- TUI syntax highlighting, input history, cursor navigation (`e643c05`)
- LSP tool, TUI pipeline view, `/plan` command (`f137648`)
- `iris plan`, `iris doc-sync`, TUI `/agents` command (`09f0ea0`)
- CoordinatorConfig, agent types, `pipeline_run()` (`755789a`)
- Gemini provider, tree-sitter, task persistence, slash commands (`bef09c1`)
- TUI model switch, compact, `iris login/logout`, MCP config (`5ea3a53`)
- Multi-provider support — auto-detect any configured API key (`7347b5a`)
- Retry logic, Claude OAuth, MCP client (`0161452`)
- Bedrock provider support (in progress)
