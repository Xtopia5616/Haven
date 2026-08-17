# Haven 架构优化实施计划

> 版本: v1.0 | 日期: 2026-08-14
> 范围: `crates/` (Rust 后端, Tauri 2) + `ui/src/` (Svelte 5 前端)
> 原则: **行为优先保持一致**；允许少许出入（日志措辞、内部命名、模块路径），但禁止改变外部契约：
> 事件 channel 名与载荷形状、DB schema、config.toml 格式、Tauri 命令名与参数、ID 格式。

---

## 背景

项目经过多次重构，当前存在四类架构问题（详见下方各步骤）：

1. **命名与语义漂移**：`Session as DbAction` 别名与 ID 规范（`act-` = 工作单元）冲突；
2. **上帝文件**：`commands.rs`（2840 行）、`agent/src/lib.rs`（6173 行）等巨型文件难以维护；
3. **同步 SQLite 在 async 上下文裸调**：`run_blocking` facade 已存在但使用不充分；
4. **配置映射手写漂移**：`AppConfig`/`Settings` 互转靠手工字段列表，易漏。

---

## 总览

| 步骤 | 内容 | 风险 | 类型 |
|---|---|---|---|
| S1 | `DbAction` → `DbSession` 命名统一 | 低 | 纯重命名 |
| S2 | `commands.rs` 按域拆分 | 低 | 纯搬移 |
| S3 | `apply_settings` 映射收敛防漂移 | 中 | 结构性 |
| S4 | ReAct 高频 DB 路径接入 `run_blocking` | 中 | 性能/一致性 |
| S5 | `agent/src/lib.rs` 拆子模块 | 中 | 结构性 |
| S6 | 前端 `+page.svelte` 拆组件 | 中 | 结构性 |

每步完成后运行对应验证（`cargo check` / `cargo clippy` / 相关测试），全部完成后跑全量验证。

---

## S1: `DbAction` → `DbSession` 命名统一

### 动机
`crates/agent/src/session.rs`（原 `crates/session/src/lib.rs`，2026-08-17 并入 haven-agent）将
`haven_memory::repositories::sessions::Session` 别名为 `DbSession`。
AGENTS.md ID 规范中 `act-` 前缀专属「工作单元」（后台任务/定时任务），会话记录叫 `DbAction` 会误导新读者。

### 改动
- `crates/agent/src/session.rs`：
  - `use haven_memory::repositories::sessions::Session as DbAction;` → `as DbSession`
  - `from_db_record(record: &DbAction)` → `&DbSession`

### 验收
- `cargo check -p haven-agent` 通过
- 仓库内无 `DbAction` 残留

### 行为影响
无（仅内部别名）。

---

## S2: `commands.rs` 按域拆分

### 动机
`crates/app-binary/src/commands.rs` 2840 行、约 60 个 Tauri 命令，涉及录音、会话、任务、历史、
模型、MCP、技能、记忆、设置、日志 10 个域。任何一处改动都要在大文件里找上下文。

### 目标结构

```
crates/app-binary/src/commands/
├── mod.rs        # 共享：AppState 导入、log_err、响应结构体、recording 共享辅助、
│                 #      公共 re-export（SkillInfo）、子模块声明
├── recording.rs  # start/stop/cancel_recording、process_transcript、get_recording_state、
│                 #      录音事件发射、附件持久化
├── session.rs    # reopen_session、get_sessions、end_session、resolve_confirmation、
│                 #      get_last_conversation、update_session_title、delete_session、
│                 #      clear_history、rollback_session、continue_session、评审相关
├── action.rs       # list_actions、cancel_action、list_action_history、delete_action
├── history.rs    # get_history、count_history、search_history(_paginated/_filtered)、
│                 #      count_history_search、export_history
├── model.rs      # get_api_key_status、check_llm_connection、list_models、discover_models、
│                 #      switch_model、set_reasoning_effort、set_web_search、router 重建
├── mcp.rs        # list_mcp_tools、reconnect_mcp、mcp_tool_call、add/update/remove/toggle_mcp_server
├── skills.rs     # list_skills、refresh_skills、set_skill_enabled、set_tool_enabled、
│                 #      open_skills_dir、execute_skill
├── memory.rs     # run_memory_maintenance、recall_memory、list_facts、add_fact、delete_fact
├── settings.rs   # get_settings、update_settings、autostart 三件套
└── log.rs        # get_log_info、read_log_tail、LogTail、日志文件解析辅助
```

### 改动规则
- 纯搬移：函数体一字不改（除 `use` 语句与可见性）。
- 共享辅助（`log_err`、`connect_and_monitor`、录音事件发射、附件持久化、`uploads_root`）放 `mod.rs` 并标 `pub(crate)`。
- `lib.rs` 引用改为 `commands::{mod}::fn`；`invoke_handler` 中的路径同步更新。
- 前端命令名不变（`#[tauri::command]` 函数名不变）。

### 验收
- `cargo check -p haven-app-binary` 通过
- `rg "commands::" crates/app-binary/src/lib.rs` 全部指向新模块
- `npm run check` 通过（前端不感知）

### 行为影响
无。

---

## S3: `apply_settings` 映射收敛防漂移

### 动机
`crates/common/src/config.rs` 中 `AppConfig`（后端全量）与 `Settings`（前端无 key 投影）是两套
平行结构，`Settings::from(&AppConfig)`（约 30 行手写）与 `apply_settings`（约 70 行手写字段拷贝）
各自维护，新增字段时极易漏掉三处之一。

### 改动（保守方案）
- `Settings` 增加 `#[serde(skip_serializing)]` 内部隐藏字段 `secret_llm: LlmConfig`（含各 key）；
  `From<&AppConfig> for Settings` 时 `secret_llm = c.llm.clone()`，同时保留现有公开 `llm`（key 置空）。
- `apply_settings` 改为：先保存 `secret_llm`，然后把 `settings.llm` 的**非空 key 字段**回填到
  `secret_llm`，整体替换 `self.config.llm`，删除手写逐字段拷贝。
  - 由于 `Settings::from` 现在携带完整 `llm`，`apply_settings` 不再需要逐字段合并非 llm 部分
    （`hotkey/session/context_limits/...` 等直接赋值，仍保持逐字段或整体替换按现实现）；
  - `Settings` 与 `AppConfig` 其余字段仍然手写，但引入 `serde(flatten)` 不可行时维持现状，
    至少把最容易漏的 llm 域收敛。
- 保留现有 key 空值不回填语义（`get_settings` 不泄漏 key，`apply_settings` 空 key 不清除）。

### 验收
- `cargo test -p haven-common config` 全部通过
- 新增测试：`apply_settings_preserves_api_keys_when_empty`、`settings_roundtrip_masks_keys`

### 行为影响
无（wire 载荷形状不变：`secret_llm` 被 `skip_serializing`）。

---

## S4: ReAct 高频路径 DB 调用接入 `run_blocking`

### 动机
`performance-review.md` #3 指出同步 SQLite（含 WAL fsync）在 Tokio runtime 内执行会阻塞 worker。
`Database::run_blocking`（`crates/memory/src/db.rs:254`）已存在。

### 范围与取舍
只覆盖**每步都会触发**的高频路径（收益最高、改动最小）：

| 路径 | 位置 | 状态 |
|---|---|---|
| 快照保存（含 branch points） | `react.rs:save_snapshot_with_branches` | ✅ 已走 `run_blocking` |
| 分支点 last_msg_at 读取 | `react.rs:save_branch_point` | ✅ 已走 `run_blocking` |
| 压缩摘要写入 episodes | `react.rs:persist_compaction_summary` | ✅ 已走 `run_blocking` |
| usage 累计写（detached） | `react.rs:record_usage_and_emit` | ✅ 已走 `spawn_blocking` |
| 消息持久化 | `lib.rs:persist_session_message` | ✅ 已走 `run_blocking` |

**明确不包**：`rollback_session` / `continue_session` / `reopen_session` 等**低频用户操作**
（`lib.rs`）——单次几十 ms 的阻塞对用户无感，包装会让异步 + 分支逻辑变得啰嗦难读。
同样不包：memory crate 仓库内部、commands 层的少量直接调用（记录到「遗留」清单）。

### 验收
- `cargo test -p haven-agent` 通过（现有 rollback/snapshot 测试覆盖行为）
- `cargo clippy` 无新告警

### 行为影响
无（同一连接池，仅线程迁移；顺序语义由调用方保持）。

---

## S5: `agent/src/lib.rs` 拆子模块

### 动机
6173 行（含 ~4200 行测试）。`AgentLayer` 承担 dispatch、rollback、任务完成消费、评审、通知等多职责。

### 目标结构
```
crates/agent/src/
├── lib.rs              # AgentLayer 核心：new/start/dispatch/process_input/session 生命周期
├── rollback.rs         # rollback_session 及其私有辅助
├── review.rs           # review_response_for_session、estimate_session_usage、get_session_for_review
├── action_completion.rs  # 后台任务完成消费循环
└── event.rs / react.rs / inference.rs / ...（现有）
```
- `impl AgentLayer` 块可拆到子模块（`impl AgentLayer` 在多个文件中合法），私有字段经
  `pub(crate)` 或 getter 暴露；优先把纯函数（无 `&self` 字段依赖）抽到子模块。
- 测试：现有 `#[cfg(test)] mod tests` 留在 `lib.rs`，新子模块各自的测试就近放置。

### 验收
- `cargo test -p haven-agent` 通过
- `cargo clippy -p haven-agent` 无告警

### 行为影响
无。

---

## S6: 前端 `+page.svelte` 拆组件（评估后降级）

### 评估结论（2026-08-14，经讨论按性价比原则决定）
`+page.svelte` 2443 行，但 script 占 ~1690 行，模板仅 ~740 行。**真正的复杂度在脚本**：
消息流状态机（`pendingChunks` / `flushPendingChunks` / 逐帧合并）、滚动跟随（`autoFollow` /
`messagesEl`，被流式 flush 路径直接操作）、模型切换、录音、事件监听全部耦合在一个组件里。
拆模板组件（`MessageList` 等）需要穿透十几个 props/回调，且 `messagesEl` 的滚动在
流式更新中被同步调用 —— **高风险、低收益**，与「行为优先一致」原则冲突。

### 降级处理
- 不拆 `+page.svelte`（模板区块已组件化：`ChatBubble`、`InputRouter`、`ConfirmationDialog`、
  `RollbackDialog`、`ContextMenu` 已是独立组件）。
- 真正的可维护性提升应来自**状态机拆分**（`streaming.js` 已承担流式 delta 逻辑，
  `reviewMessages.js` 已承担消息投影），而非模板组件化。
- 记录到「遗留」清单：若后续继续，优先把 `+page.svelte` 的滚动跟随与消息列表抽成
  `MessageList.svelte`（props: `messages`/`autoFollow`，回调: `onContextMenu` 等），
  且必须用现有 `reviewMessages.test.js` 级别的单测覆盖后再动。

### 验收
- `npm run check` 通过
- `npm run test:run` 通过（282 tests）
- 行为影响：无（未改动）

---

## 遗留清单（本计划不做，记录待办）

- 事件三通道统一到 `EventBus`（action:* 事件接入 `TauriEmitter::channel` 注册表）——改动面大，
  涉及前端归一化兜底，单独排期。
- `haven-tools` 依赖收敛：`file.rs` 的 `LlmRouter` 摘要/视觉端口化（`Summarizer` trait）。
- `facts.rs`（2797 行）/ `router.rs`（2661 行）/ `bg.rs`（2243 行）的进一步拆分。
- 其余 ~220 处同步 DB 调用点的 async 化（S4 只覆盖每步高频路径；低频用户操作如
  `rollback_session` 明确不包，避免降低可读性）。
- `+page.svelte` 的消息列表 + 滚动跟随组件化（S6 评估后降级，见上）。

---

## 全量验证

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd ui && npm run check && npm run test:run
```

---

## 变更记录

| 日期 | 步骤 | 说明 |
|---|---|---|
| 2026-08-14 | S1–S5 | 完成：命名统一、commands 拆分、apply_settings 收敛、S4 确认高频路径、rollback.rs 抽取 |
| 2026-08-14 | S6 | 评估后降级（脚本状态机耦合深，拆组件风险高收益低，记录待办） |
| 2026-08-14 | 评审修复 | 恢复 rollback fallback 语义；LLM/工具全量 trace 降级为长度摘要；`max_response_tokens` 默认 128k 且 router 按 context_window clamp；修复 rollback.rs mojibake |
| 2026-08-14 | 测试修复 | `dispatcher_panicked_handler_marks_error` flaky：DB error 写入与内存 slot 释放间存在窗口，断言改轮询；回调 `try_lock().unwrap()` 改 `std::sync::Mutex` 消除锁竞态 |
