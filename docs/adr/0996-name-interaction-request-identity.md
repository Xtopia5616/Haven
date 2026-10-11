# ADR 0996：统一交互请求身份字段

## 状态

已采纳并实施；Rust workspace、UI、IPC 与文档门禁通过。

## 背景

`InteractionRequest` 覆盖 Ask、Agent 确认、AppCommand 确认与定时确认。它们共享一条生命周期，但身份落在不同的既有 ID 空间：Ask 使用其 `step-*`，确认使用 `conf-*`。内部 Rust、durable lifecycle event、Tauri projection 和 UI view 此前都把身份称为 `id`，调用方只能再按 kind 推断实体；`interaction_cleared` 的 `{ "ids": [...] }` 也没有说明数组成员是请求身份。

这类事件保存在 append-only `session_events` 中。历史 payload 必须能在不重置用户数据的情况下继续 replay。

## 决定

- Rust `InteractionRequest` 和 App `InteractionRequestedEvent` 统一使用 `request_id`。Ask 继续复用 `step-*`，确认继续使用 `conf-*`；不增加 ID 前缀或身份生成路径。
- `interaction_requested`、`interaction_resolved` 的新 payload 只写 `request_id`；Rust replay reader 只为历史 durable event 保留反序列化 `id` alias。
- 新 `interaction_cleared` payload 使用 `request_ids`；Agent replay 和 confirmation-batch replay 接受历史的 `ids`，但所有新写入都使用 `request_ids`。
- UI event/resume wire parser 只接收 Rust 的 `request_id`；renderer `InteractionRequest` 统一使用 `requestId`，reducer action 和恢复保留列表也使用 `requestId` / `preserveRequestIds`。
- SQLite schema、resolve command 的 `request_id`、owner 路由、请求状态及 Ask transcript 身份关联保持不变。

## 替代方案

- 只在 UI 做别名映射、保留 Rust 与 durable event 的泛名 `id`：拒绝。该字段贯穿持久 replay 与跨层 DTO，保留泛名会使实体职责继续依赖 kind 推断。
- 删除历史 reader 兼容并要求重置数据库：拒绝。仅需 replay reader 的反序列化 alias；新事件始终输出 canonical 字段，因此不需要 schema 迁移或清空数据。
- 为 Ask 与确认各自引入新身份：拒绝。Ask 的 `step-*` 与确认的 `conf-*` 均已有稳定生命周期和调用约定，改前缀会增加迁移与关联复杂度。

## 影响与验证

新的 durable interaction payload 与 Tauri event/resume contract 使用 `request_id`，UI view 使用 `requestId`。老 durable payload 的 `id` / `ids` 只在 Agent replay 中兼容；UI 不接受旧 `id`。Generated IPC contracts、命名规范、IPC 清单和架构路线图同步更新。

验证通过：`cargo check --workspace --locked`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo fmt --all -- --check`、`corepack pnpm run check`、`corepack pnpm run test:run`（137 个文件、1095 项通过）、`scripts/check-ipc-contracts.ps1`、`scripts/check-ipc-events.ps1`、`scripts/check-adr-index.ps1` 与 `git diff --check`。最后一次全量 Rust 测试包含身份字段改名和恢复 Ask 断言修正；其后 Memory 新事件 fixture 的 `request_id` 命名另经对应单测确认。

## 回滚

可恢复 Rust/UI 原字段名并重新生成 IPC contracts。历史 reader alias 可随旧字段恢复而删除；durable event 存储与数据库 schema 未改变，不需要数据重置。
