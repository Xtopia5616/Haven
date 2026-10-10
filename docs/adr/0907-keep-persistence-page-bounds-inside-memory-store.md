# 0907：由 Memory store 持有持久化分页上限

## 状态

已接受并实现（2026-10-11）。

## 背景

Fact/summary extraction outbox 与 session recovery 都通过 Memory 的 keyset page 读取。具体上限由 Memory SQLite 查询定义，但生产 Agent 需要导入 `MAX_MEMORY_OUTBOX_PAGE_SIZE` 和 `MAX_MEMORY_SESSION_ID_PAGE_SIZE`，再把上限传给 typed store。这样 Agent 的 scanner 就依赖了持久化分页实现参数；修改页容量也要求同步调用层。

Agent 仍负责 high-water snapshot、cursor 推进、恢复和公平调度。这些是 runtime 生命周期与工作策略，不应迁入 Memory store。Memory store 只需保证每次 page 有固定上限，并隐藏该上限。

## 决定

- `MemoryStore` 的 extraction outbox page 操作不接受调用方传入的 `limit`，始终使用 Memory 内部定义的 bounded page size。
- `SessionStore` 的 session ID page 操作同样不接受 `limit`，始终由 Memory 决定单页上限。
- 两个 page-size 常量只在 `test-support` 下从 crate root 导出，供原始数据库 fixture 与性能基准检查容量；默认生产 API 不暴露它们。
- Agent 继续拥有 high-water、cursor、取消和 recovery loop；仅移除对 Memory 页容量的直接依赖。

## 影响

- store API 按语义提供固定大小的 bounded page；调用方不能选择超出上限的查询，也无需重复传递同一常量。
- 单页最大行数、keyset 顺序、high-water 边界、取消和恢复行为不变。
- 不改变 SQLite schema、marker 格式、持久化内容或用户可见行为。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `pwsh -NoProfile -File scripts/check-adr-index.ps1`

## 替代方案

- 保留 Agent 传入 Memory 上限常量：拒绝，因为 scanner 不需要决定数据库页容量。
- 将 high-water、cursor 和完整扫描 loop 一并移入 Memory：拒绝，因为恢复调度属于 Agent runtime，且 Memory store 不应接管后台任务策略。
- 让生产调用方使用无界查询：拒绝，因为大队列/长 session history 会扩大单次 SQLite 工作量并损害取消响应。
