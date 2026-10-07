# ADR 0736：ToolRun 通知事件复用 wire enum

## 状态

已采纳并实施。

## 背景

Agent 的 `ToolRunCompletionNotification` producer 已使用 `ToolRunNotificationSource::{Background, Scheduled}` 和 `Option<ToolRunCompletionStatus::{Completed, Failed}>`。调用点来自后台 ToolRun result delivery 与 scheduled ToolRun lifecycle；App `event_bridge` 将它们映射到 `notification:show` 的 App-owned `AgentNotificationEvent`。但 DTO 把 source/status 降为 `Option<String>`，bridge 通过两个 `as_str` helper 再生成相同的小写值。

前端 `AgentNotificationPayload` 手写了 `toolRunKind` 与 `toolRunStatus` union，`mapAgentEvent` 又独立检查字面量。根布局消费该通知，交给 ToolRun completion gate 和应用内/Windows 通知。UI mapper 要求 background 通知携带真实 `session_id` 与 terminal status；scheduled 通知不带 status。未知 discriminator/source/status 会丢弃该事件。普通 Agent `Notification` 共用 channel 但不带 ToolRun marker，可选择是否提供 session 关联。

这是内存中的 UI/桌面通知事件，不属于 session durable event stream，也没有数据库、配置或重启恢复语义。Tauri emit 失败按现有路径记录 warning；不做重试或持久补发。

## 决定

- App `AgentNotificationEvent` 的 `tool_run_kind` 改用 App-owned `ToolRunKindDto`；新增 `ToolRunCompletionStatusDto::{Completed, Failed}` 表达通知专用终态子集；`notification_kind` 继续使用 `AgentNotificationKind`。
- event bridge 显式将 Agent producer enums 映射为 App wire enums。删除仅用于该字符串桥接的 Agent `as_str` helpers。
- IPC generator 显式导出通知 discriminator/source/status enum 与 runtime value arrays。前端 payload interface 引用生成的类型，mapper 从生成的 value arrays 校验，而不是另存 literal unions/guard 列表。
- 保留 JSON key、snake_case 值、通用通知行为、background/scheduled 的必填与省略策略、通知 gate 和失败处理。

## 替代方案

- 保留开放 `String` 并继续手写 UI union：拒绝。已有 App wire enum owner，string 字段让事件 DTO 和生成类型失去值域约束。
- 通知 status 复用完整 `ToolRunStatus`：拒绝。通知 producer 仅允许 `completed` / `failed`；把等待、运行中、取消状态纳入该事件会扩大合法输入。
- 直接在 wire DTO 中使用 Agent enums：拒绝。`AgentEvent` 是领域 producer contract；App DTO 是 Tauri wire owner，bridge 应显式映射。

## 影响与验证

Rust DTO 在编译期限制 ToolRun source/status；TypeScript payload 类型和 unknown-value runtime validation 共享 generated enum owner。事件 JSON 保持 `background`、`scheduled`、`completed`、`failed` 原值，缺省字段继续省略；畸形外部事件仍由 UI mapper 丢弃。无持久数据、配置、授权或恢复变化，不需要数据重置。

验证：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked` 与 app-binary notification bridge 定向测试通过；UI `check`、`test:run`（125 files / 992 tests）和 `build` 通过；`scripts/check-ipc-contracts.ps1`（80 handlers）、`scripts/check-ipc-events.ps1`（35 channels）、`scripts/check-adr-index.ps1`（719 ADRs）与 `git diff --check` 通过。

## 回滚

如回滚，需恢复 App notification DTO 的开放字符串、event bridge 的 `as_str` 映射、Agent helper、手写 UI literal unions/guard、generated contract、event checker、文档和本 ADR 索引；不涉及持久数据迁移。
