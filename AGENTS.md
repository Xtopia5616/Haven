# Haven Project Guide

所有变更还必须遵守 `docs/development-standards.md` 的架构、契约、安全、测试与变更管理要求；本文件保留面向 agent 的具体执行规则。

## Project
Haven is a voice assistant for Windows PC built on the Pi Coding Agent (ReAct loop) architecture.
Tech stack: Rust (Tauri 2) backend, Svelte 5 frontend.

固定开发工具链：Rust 1.98.0、Node.js 24.20.0、pnpm 11.24.0；版本分别由 `rust-toolchain.toml`、`.node-version` 和 `ui/package.json` 固定。

## Test Workflow

### Rust Backend
```sh
# Run all workspace tests
cargo test --workspace --locked

# Run tests for a specific crate
cargo test --locked -p haven-agent
cargo test --locked -p haven-memory -- preferences

# Run with output
cargo test --workspace --locked -- --nocapture

# Run clippy
cargo clippy --workspace --locked -- -D warnings

# Coverage (requires cargo-tarpaulin)
cargo tarpaulin --out Html --output-dir target/coverage
```

### UI Frontend
```sh
cd ui

# Watch mode
corepack pnpm run test

# Single run
corepack pnpm run test:run

# With coverage
corepack pnpm run test:coverage

# Svelte type check
corepack pnpm run check
```

### Codex Worktree Build Cache
- Codex worktrees are supported; Kilo/Kilocode worktrees and commands are retired.
- `.cargo/config.toml` disables Rust incremental compilation to prevent unbounded per-worktree cache growth.
- To reclaim generated artifacts from the current checkout and Codex worktrees, run `pwsh -NoProfile -File scripts/cleanup-codex-targets.ps1 -IncludeCurrentWorktree -Confirm:$false`.

## Cargo Aliases (via .cargo/config.toml)
- `cargo t` — `cargo test`
- `cargo ts` — `cargo test -- --nocapture`
- `cargo c` — `cargo check`
- `cargo cl` — `cargo clippy -- -D warnings`

## Test Conventions
- Use `#[cfg(test)] mod tests { ... }` in each source file for unit tests
- Use `crates/*/tests/` for integration tests
- Use `Database::open_in_memory()` for SQLite tests in haven-memory
- Mark test-only constructors with `#[cfg(test)]`
- Use `tokio::test` for async tests

## ID 规范（统一 ID 格式与命名）

### 实体 ID 格式
所有实体 ID 统一为 `{prefix}-{uuid32}`（前缀 + 连字符 + 32 位小写 hex，simple UUID，不含连字符）。
前缀表：

| 前缀 | 实体 | 位置 |
|---|---|---|
| `ses-` | 会话 sessions.id | `haven_memory` |
| `msg-` | 消息 messages.id；记忆条目 memory_items.id（compaction 摘要等）与 transcript 共用该 ID 空间 | `haven_memory` |
| `step-` | 步骤 session_steps.id | `haven_memory` |
| `fact-` | 记忆边 memory_edges.id（SPO；原 facts） | `haven_memory` |
| `node-` | 记忆节点 memory_nodes.id | `haven_memory` |
| `act-` | 工作单元 actions.id（后台任务 kind=`background` + 定时任务 kind=`scheduled`） | `haven_tools` |
| `usage-` | 单次 LLM 调用用量明细 llm_usage.id | `haven_memory` |

以下前缀均为**进程内** ID，不落库：

| 前缀 | 实体 | 位置 |
|---|---|---|
| `conf-` | 安全确认请求 | `haven_agent` |
| `rec-` | 录音会话（一次录音一个 id，`recording:started`/`transcription:*` 事件共用） | `haven_app` |
| `file-` | 临时文件名 | `haven_app` |
| `call-` | provider 返回空 tool_call_id 时的本地兜底 | `haven_agent` |

规则：
- **生成一律用 `haven_common::types::new_id(prefix)`**，禁止手拼 UUID。
- **X12 写路径**：`ReActSnapshot.events`（`sessions.react_state`）是 append-only 权威；`messages` / `session_steps` 是物化投影。ReAct 循环内 assistant/thought/ask/reasoning 内容行只从 `apply_transcript`（→ `project_chat_message`）写出；禁止平行 `persist_session_message`。例外（须文档化）：ingress 用户 seed（崩溃安全，`UserInject` 带 `message_id` 时不再写 messages）、error partials（有意不进 events，靠 `last_msg_at` 截断）、terminal action-result（无活 loop）、confirm/ask 重提示等 UI-only 通知气泡。
- **内容行与执行行共用 id**（同一实体在 `messages` 与 `session_steps` 各存一面，内容只落 messages，另一面只存执行态）：assistant thought 的消息行与 thought 步骤行共用 `step-*` id（流式气泡 id 按 `step` 前缀 mint，`session_steps.thought` 列新数据不再写入）；补充输入/steering 的 thought 步骤行与用户消息行共用 `msg-*` id（消息行先落库，步骤行以 `message_id` 复用）；ask 问题消息行与 ask 步骤行共用 `step-*` id（问题文本在 `apply(ToolResult)` 投影到 messages，resume 的 snapshot-less 重建跳过 `action_tool='ask'` 步骤）。旧库行保留旧格式，前端按 id 关联、内容匹配仅作 legacy 兜底。
- **resume 恢复补充输入按时间不按内容**：有 snapshot 时 `events` 唯一权威；无 snapshot 可从投影重建；坏 snapshot hard fail。`ReActSnapshot` 带 `saved_at`，resume 时仅当 executor 队列为空（崩溃/重启）才把 `created_at > saved_at` 的 user 消息重新排队；禁止再引入内容比对去重。前端提交在 `submitTranscript` 有 in-flight 锁（并发提交共享同一 promise），后端不再对用户输入做内容去重。
- **Rollback 双时钟**：`BranchPoint.event_cursor` 截断 events；`last_msg_at` 截断投影表。每次投影写必须 `note_last_msg_at`。
- Rust/DB/事件字段统一 snake_case `xxx_id`（`session_id`、`action_id`、`message_id`…）；前端在边界转 camelCase `xxxId`。
- 术语：**session** = 对话（ReAct 主实体）；**action** = 工作单元（后台任务/定时任务，`actions` 表 kind 区分）；任务/作业/提醒统一叫任务，UI 文案一律「会话」「任务」「后台任务」「定时任务」。
- 实体 ID newtype 集中在 `haven_common::types`（`id_newtype!` 宏生成，`struct X(pub String)`，serde 按普通字符串序列化）：目前只有 `ConfirmId`/`SessionId` 在运行时被使用，其余实体继续用 `String`；新增真正需要类型隔离的实体 ID 时再补 newtype，不要提前定义未使用的类型。
- 序号类字段（u64 代次，非持久实体）：`run_id`（run 实例）、`gen_id`（流式代次）、MCP JSON-RPC `next_id`，保持现有命名并加文档说明。
- 外部 ID（LLM `tool_call_id`、模型 ID、MCP `Mcp-Session-Id`）保持 provider 格式，不套用本规范。
- kv_store key 用 `domain.key` 风格（如 `fact_extraction.{session_id}`、`fact_extraction_pending.{session_id}`），内嵌的实体 ID 必须是规范格式。
- 步骤计数统一叫 `step_number`（事件/UI/DB 列名一致）。
- 数据库 schema 由 `haven_memory::schema::init_schema` 管理：当前 `SCHEMA_SQL` 幂等建表并写入 `SCHEMA_VERSION`；旧版本数据库不做运行时迁移，直接按 `docs/release-and-reset.md` 重置。`user_version` 高于或不同于本二进制支持版本时拒绝打开。演进原则与剩余重构工作见 `docs/stability-refactor-plan.md`。

## 通知 / 日志 / 错误处理规范

统一规范见 `docs/conventions.md`（v1.3）：前端 `logger.*` 禁止裸 `console.*`；后端 `tracing` + 命令错误走 `log_err(ctx, e)`；通知双通道（应用内 toast / Windows），系统事件集中在 `+layout` 经 `addNotification`，用户操作可页面直调；Tauri 命令统一 `Result<T, String>`。已知漂移与优化项见该文档 §6。

## 命名规范

统一规范见 `docs/naming.md`（Rust/前端各层命名、缩写大小写、跨层 snake↔camel 边界、自查清单）。要点：Rust 文件/模块 snake_case、类型 PascalCase、常量 UPPER_SNAKE；Svelte 组件文件名=组件名（PascalCase）、JS 模块 camelCase、store 尾缀 `Store`；缩写整词统一（`stt`/`ocr`/`tts`）；跨层只在边界转换。

## Git 提交
完整流程见 `docs/git-workflow.md`。在本项目中，每轮逻辑改动完成且适用门禁通过后，直接提交，不等待用户再次确认；不自动推送远端。

1. 用 `git status --short`、`git diff` 确认范围，保留用户已有的无关修改。
2. 运行适用门禁：`/check`、`/test`、`/test-ui --run`；跨 crate/跨端改动还要运行项目要求的完整检查。
3. 使用 `git add -- <明确路径>` 精确暂存，不默认使用 `git add .` 或 `git add -A`。
4. 提交前运行 `git diff --cached --check` 并复核 staged diff，确认没有密钥、用户数据或生成产物。
5. 使用 `<type>(<scope>): <imperative summary>` 提交，并在提交后复核 `git status --short` 和 `git log -1 --oneline`。
