# ADR 0306：Tools admin capability 通过 typed stores 注入

- 状态：Accepted
- 日期：2026-09-25
- 范围：`AdminContext`、内置 MemoryTool 装配、admin diagnostics 的 session 读取
- 关联：[ADR 0302](0302-tools-memory-fact-store-port.md)、[ADR 0249](0249-session-store-session-record-reads.md)、[ADR 0305](0305-action-service-action-store-port.md)

## 背景

`AdminContext` 将 `Arc<Database>` 暴露给 Tools。admin diagnostics、sessions 和 errors 因此直接调用同步 session 查询；builtin 装配还从该通用 facade 创建 `MemoryFactStore`。这让管理能力上下文携带超出其职责的数据库权限，也让 Tools 绕过 Memory 既有的异步 typed ports。

`SessionStore` 已提供异步 `list_history` 与 `count_history`，`MemoryFactStore` 已覆盖 MemoryTool 使用的事实查询与写入。两者是所需且足够的 capability 边界。

## 决定

1. `AdminContext` 删除 `db`，仅为 admin session 查询持有 `Option<SessionStore>`，并为 MemoryTool 持有 `Option<MemoryFactStore>`；不暴露 `Database` 或 `Arc<Database>`。
2. app-binary 组合根创建并共享 `SessionStore` 与 `MemoryFactStore`。`ApplicationRuntime` 接收已有的 `MemoryFactStore` handle；admin context 将对应 handles 显式交给 Tools。`builtin/mod.rs` 只传递 `memory_facts`，不从数据库构造 store。
3. `diagnostics_status` 通过 `SessionStore::list_history(50, 0)` 统计最近 50 条状态，并通过 `count_history` 计算全部会话总数。列表失败仍写 warning、计数失败仍写 warning 并按 `0` 输出；store 不可用时继续输出 `{"unavailable":true}`。
4. `sessions` 与 `errors` 通过 `list_history` 读取，保留默认 limit 10、limit clamp 到 1–50、`created_at DESC` 排序，以及 errors 先限量再筛选 Error 状态的既有顺序。查询错误继续向 operation error 映射。
5. Config-only context 仍不配置 session 或 memory capability。此边界不改变任何 provider-facing tool name、argument、response wire contract 或数据库 schema。

## 影响与验证

- 无 schema、配置、IPC 或 provider wire contract 变化；无需数据重置。
- 回归测试覆盖缺失 SessionStore 时 diagnostics/sessions/errors 的 unavailable 行为、最近 50 条状态统计与全量计数、session 排序/limit、errors 的筛选顺序和字符计数。
- 生产 `admin.rs` / `admin_services.rs` 不含 `Database`、`run_blocking` 或 `conn`；builtin 不负责创建 `MemoryFactStore`。
- 验收命令：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo test --locked -p haven-memory`、`cargo test --locked -p haven-tools`、`cargo test --locked -p haven-agent`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`。

## 替代方案

- 保留 `Arc<Database>`，只把当前查询包进 helper：仍然授予 Tools 通用数据库访问，不满足 capability 边界，拒绝。
- 为 admin 新建重复的 session repository：`SessionStore` 已提供所需异步历史读端口，会造成重复职责，拒绝。
- 让 `builtin/mod.rs` 继续从 `AdminContext` 的数据库创建 `MemoryFactStore`：继续把 store 的构造和底层数据库权限留在 Tools，拒绝。

## 回滚

代码回退可恢复 `AdminContext.db` 及其同步查询调用，并恢复 builtin 创建 `MemoryFactStore`。本 ADR、索引与架构/路线图记录可同时删除。无数据格式或迁移要求。
