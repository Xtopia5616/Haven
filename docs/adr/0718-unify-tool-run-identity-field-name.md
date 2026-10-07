# ADR 0718：统一 ToolRun 身份字段名

## 状态

已采纳并实施。

## 背景

ToolRun 在不同层使用了多个身份字段名：持久行和 board view 使用 `id`，Tauri `ToolRunEvent` 与前端 store 也把 ToolRun 身份放在泛名 `id` 下；schedule set 输出 `id`、cancel 输出 `cancelled: <id>`，scheduled status 又同时输出 `id` 与 `tool_run_id`。这让同一个持久实体的身份依赖上下文猜测，并造成 UI renderer 读取了 contract 已不承认的字段。

## 决定

- Rust ToolRun 行、view、生命周期测试投影、Tauri payload 与 ToolRun JSON 结果统一使用 `tool_run_id`；前端 IPC mapper 在单一边界将其转换为 `toolRunId`，UI view/store 只使用该字段。
- 删除 schedule status 与 lifecycle 测试投影中的 `id` 别名；schedule set/cancel 都以 `tool_run_id` 返回身份，取消结果不再把 ID 塞入 `cancelled` 字段。
- `tool_runs.id` 是现有 SQLite 物理主键列，保留不变；Repository 在读取时将其映射为 `tool_run_id`。Tauri event envelope 的数值 `event.id` 属于 event 实例，不是 ToolRun 身份，也保留不变。
- 不接受旧 `id` payload，也不保留旧输出形状。没有持久数据迁移或数据库重置需求。

## 替代方案

- 仅改 Tauri DTO 而保留 Repository/view 的泛名：拒绝。相邻层仍需在同一 ToolRun 实体上反复猜身份语义。
- 同时输出 `id` 与 `tool_run_id`：拒绝。双字段制造重复身份来源并掩盖未迁移消费者。
- 将 SQL 列改为 `tool_run_id`：拒绝。本次只统一应用层字段语义；物理列无需变更，且 schema/reset 不属于本问题。

## 影响与验证

此变更破坏 Tauri ToolRun payload 与 schedule 工具结果中的旧身份 key，前端同步只接受规范字段。更新生成 IPC 类型、Rust/TypeScript consumers 与契约测试；不改 ToolRun 生命周期、数据库 schema、主键值或 event envelope。

验证通过：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`；UI `corepack pnpm run check`（0 errors、0 warnings）、`corepack pnpm run test:run`（124 files、986 tests）与 `corepack pnpm run build`；`scripts/check-ipc-contracts.ps1`（81 handlers）、`scripts/check-ipc-events.ps1`（35 channels）、ADR index（701 records）及 `git diff --check`。ToolRun crate 定向复验 `cargo test --locked -p haven-tools`（801 passed、2 ignored；integration 7 passed）也通过。

## 回滚

若需要回滚，须同时恢复 Rust `ToolRunEvent`、生成 IPC contract、Repository/view 字段与所有 UI/test consumers；数据库 schema 无需回滚或重置。
