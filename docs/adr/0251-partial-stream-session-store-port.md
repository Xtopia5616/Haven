# ADR 0251：partial stream 通过 SessionStore 持久化

- 状态：已采纳（2026-09-24）
- 范围：`haven-agent::PartialStore` 与 `haven-memory::SessionStore`
- 关联：[ADR 0207](0207-session-store-replay-boundaries-and-durable-ui-sequences.md)、[ADR 0249](0249-session-store-session-record-reads.md)

## 背景

`PartialStore` 已经是流式草稿的生命周期协调点，但它仍直接持有
`Arc<Database>`，并在 checkpoint、promote、discard 三条路径分别调用
`partial_messages` 的底层操作。这样 Agent 仍然知道 scratch projection 的存储实现，
也让同一个 `SessionStore` 在 `SessionSupervisor` 中被重复旁路。

## 决定

1. `SessionStore` 提供 `upsert_partial_stream`、`promote_partial_stream` 和
   `discard_partial_stream` 三个最小 typed 操作。
2. `PartialStore` 只持有 `SessionStore`；per-session lock、generation、去重、
   stale checkpoint 丢弃和错误语义继续由 `PartialStore` 所有。
3. promote 继续委托既有原子 take/insert 与时间戳保护；partial scratch 不推进
   `last_msg_at`，不新增 schema、事件或第二个 durable authority。
4. `SessionSupervisor` 将同一个 `SessionStore` 实例共享给 PartialStore，测试只在
   `SessionStore` 边界验证数据库语义。

## 影响与验证

这只收窄 Agent 到 Memory 的存储依赖，不改变 partial generation、checkpoint/promote/
discard 时序、恢复行为或 transcript projection。验证覆盖 partial checkpoint、空内容、
原子 promote、过期/重复 promote 和 discard；同时运行 Agent/Memory 测试、workspace
严格 Clippy 与全 workspace 测试。

## 回滚

回退本切片提交并恢复 `PartialStore` 的 `Arc<Database>` 构造即可；没有 schema、IPC 或
用户数据格式变化，无需数据库重置。
