# Haven Project Guide

## Project
Haven is a voice assistant for Windows PC built on the Pi Coding Agent (ReAct loop) architecture.
Tech stack: Rust (Tauri 2) backend, Svelte 5 frontend.

## Test Workflow

### Rust Backend
```sh
# Run all workspace tests
cargo test

# Run tests for a specific crate
cargo test -p haven-agent
cargo test -p haven-memory -- preferences

# Run with output
cargo test -- --nocapture

# Run clippy
cargo clippy -- -D warnings

# Coverage (requires cargo-tarpaulin)
cargo tarpaulin --out Html --output-dir target/coverage
```

### UI Frontend
```sh
cd ui

# Watch mode
npm run test

# Single run
npm run test:run

# With coverage
npm run test:coverage

# Svelte type check
npm run check
```

### Kilo Commands
- `/test [crate] [filter]` — run Rust tests
- `/test-ui [--run|--coverage|--e2e]` — run UI tests
- `/check [fmt|rust|clippy|ui]` — run static analysis
- `/coverage [--ci|--ui]` — generate coverage reports

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
| `task-` | 任务 tasks.id | `haven_memory` |
| `msg-` | 消息 messages.id；记忆片段 memory_episodes.id 与消息共用该 ID 空间 | `haven_memory` |
| `step-` | 步骤 task_steps.id | `haven_memory` |
| `fact-` | 长期记忆事实 facts.id | `haven_memory` |
| `act-` | 活动（统一的后台作业与提醒）id：`reminders.id`、后台作业 id | `haven_tools` |
| `conf-` | 安全确认请求（进程内，不落库） | `haven_task` |
| `rec-` | 录音会话（一次录音一个 id，`recording:started`/`transcription:*` 事件共用，进程内） | `haven_app` |
| `file-` | 临时文件名 | `haven_app` |
| `call-` | provider 返回空 tool_call_id 时的本地兜底 | `haven_agent` |

规则：
- **生成一律用 `haven_common::types::new_id(prefix)`**，禁止手拼 UUID。
- Rust/DB/事件字段统一 snake_case `xxx_id`（`task_id`、`message_id`…）；前端在边界转 camelCase `xxxId`。
- 实体 ID newtype 集中在 `haven_common::types`（`id_newtype!` 宏生成，`struct X(pub String)`，serde 按普通字符串序列化）：目前只有 `ConfirmId`/`SessionId` 在运行时被使用，其余实体继续用 `String`；新增真正需要类型隔离的实体 ID 时再补 newtype，不要提前定义未使用的类型。
- 序号类字段（u64 代次，非持久实体）：`run_id`（run 实例）、`gen_id`（流式代次）、MCP JSON-RPC `next_id`，保持现有命名并加文档说明。
- 外部 ID（LLM `tool_call_id`、模型 ID、MCP `Mcp-Session-Id`）保持 provider 格式，不套用本规范。
- kv_store key 用 `domain.key` 风格（如 `fact_extraction.{task_id}`），内嵌的实体 ID 必须是规范格式。
- 步骤计数统一叫 `step_number`（事件/UI/DB 列名一致）；`step_index` 已废弃。
- 存量数据迁移见 `haven_memory::migrations` §ID 块（幂等，仅当存在未加前缀的 task id 时执行）。

## 通知 / 日志 / 错误处理规范

统一规范见 `docs/conventions.md`（前端 `logger.*` 禁止裸 `console.*`；后端 `tracing` + 命令错误走 `log_err(ctx, e)`；通知事件驱动，前端只经 `addNotification(msg, type, duration)`；Tauri 命令统一 `Result<T, String>`）。

## Before Committing
1. Run `/check` (fmt → check → clippy → svelte-check)
2. Run `/test` (all workspace tests pass)
3. Run `/test-ui --run` (UI tests pass)
