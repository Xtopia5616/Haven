# ADR 0529：单一会话生命周期事件与 Memory response DTO

## 状态

已采纳并实施（2026-10-06）。

## 背景

会话完成和错误以前分别发送 `session:completed` / `session:error`，随后再投影一条 `session:updated`。聊天页、全局通知和记忆会话列表各自订阅多个 channel；聊天页需要 `occurrence_id` 才能判断终态清理是否已经执行。重复事件、单条发送失败和消费顺序都扩大了生命周期协议和 handler 状态。

Memory 的 `list_facts` 与 `add_fact` 直接返回 repository `Fact`。这让存储类型同时承担 Tauri wire contract，仓储字段变更可能在没有 App 边界审查的情况下改变 renderer payload。ADR 0357 已指定 Rust 为权威来源，但没有要求 repository entity 本身就是 wire DTO。

## 决定

1. 所有会话生命周期变化只通过一个 `session:lifecycle` channel 发送。Rust `SessionLifecycleEvent` 是带 `type` 标签的唯一 wire enum：`created`、`updated`、`completed`、`error`、`title_updated`、`deleted`。
2. `updated.status` 限于 `pending`、`running`、`paused`。终态使用独立的 `completed` / `error` variant，并在同一 payload 内要求 `reason` / `error` 字符串；这些值由 App 发送前净化。普通 status update 不再表达终态。
3. `TauriEmitter` 对每个 Agent lifecycle event 只 emit 一次，不再生成 secondary `session:updated`，也不 mint `occurrence_id`。删除、清空和改标题命令也使用相同 channel 和 DTO。dispatcher 的终态错误发布 owner 仍按 ADR 0511 保持唯一。
4. 聊天页、根布局通知/忙碌状态和 MemoryView 会话列表都订阅 `session:lifecycle`，各自按 discriminant 处理所需投影。MemoryView 先完成监听注册再读取首屏列表，避免首屏查询期间漏掉 lifecycle 更新。聊天页只在一个 terminal branch 执行 transcript/ask/preview/session-memory cleanup；列表与通知不通过额外 lifecycle channel 推导终态。
5. `list_facts` 与 `add_fact` 显式把 Memory repository `Fact` 映射为 App-owned `MemoryFactResponse`，source reference 映射为 `MemoryFactSourceRef`。Rust DTO 是 Tauri wire shape 权威；生成器导出 TypeScript interface，UI 的 `Fact` / `FactSourceRef` 仅作生成类型别名。
6. 不提供旧 channel、旧 payload shape 或 repository entity 的兼容层。`occurrence_id` 不再是新事件 identity；历史 ADR 0349 / 0386 对双 channel 与 occurrence 去重的实现决策由本 ADR 取代，ADR 0511 的单一错误发布 owner 继续有效。

## 替代方案

- 保留主终态 channel 并让消费者继续听 `session:updated`：拒绝。它继续保留两种终态表达和重复清理问题。
- 所有 consumer 合并到一个集中 reducer 后再分发：拒绝。聊天状态、toast/busy state 和 memory list refresh 是不同副作用 owner；共享的是 wire event，不需要再建立全局事件 store。
- 继续直接序列化 repository `Fact`：拒绝。Rust 仍是权威，但 IPC field allowlist 应归 App response DTO 所有。
- 在 UI 维护第二份完整 `Fact` interface：拒绝。静态类型直接从 Rust `MemoryFactResponse` / `MemoryFactSourceRef` 生成。

## 影响与验证

- 此变更破坏旧 Tauri event channel 和 event payload contract；renderer 与 App 必须一起升级。无旧 event alias 或运行时 fallback。
- 不改数据库 schema、durable session events、Memory repository schema、持久化內容或 command names，不需要清理/重置用户数据。
- lifecycle mapper 和 Rust event bridge tests 覆盖同一 channel、discriminant、required terminal details、非终态 update status 与删除全量 sentinel；UI tests 覆盖单一 channel 注册和聊天页终态清理单次执行。MemoryView 按统一 lifecycle event 刷新列表，并就地更新标题。
- 验收通过：`cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`corepack pnpm --dir ui run check`、`corepack pnpm --dir ui run test:run`（122 files / 974 tests）、`corepack pnpm --dir ui run build`、`scripts/check-ipc-events.ps1`（35 channels）、`scripts/check-ipc-contracts.ps1`（79 handlers）、`scripts/check-adr-index.ps1` 与 `git diff --check`。

## 回滚

回滚本切片的代码、生成物、测试、IPC/架构文档与本 ADR；不需要数据回滚或重置。回滚会恢复多个会话事件 channel 和 repository `Fact` 的直接 IPC 投影，因此必须同时恢复其对应消费者和合同检查。
