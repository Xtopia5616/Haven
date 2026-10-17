# ADR 1306：由私有模块拥有 SessionStore rollback 与 usage 事务

状态：已接受并实施（2026-10-17）。

## 背景

`SessionStore` 的非 event-core API、append/projection 与 replay/read 已拆到私有模块。rollback marker、投影 cutoff、usage compensation、branch-point 和 recovery marker 仍与 facade 代码混排；这些操作共享 event sequence 与 message/step/usage 投影时钟，必须保留各自现有原子边界。

## 决定

1. 将 rollback target、rollback marker、replacement transcript、projection truncation 和 usage compensation 放入私有 `rollback` 模块。
2. 将 usage-recorded/usage-discarded event 与 `llm_usage`/session aggregate 写入放入私有 `usage_events` 模块。usage event 与对应投影仍在同一事务；回滚删除 projection 时，discard event 与 usage aggregate rebuild 仍在原事务完成。
3. 将 branch-point 与 recovery-persistence marker 的追加、查询放入私有 `branch_events` 模块，继续复用同一 `SessionStore`、event log 与 replay reader。
4. 将 Memory event consumer cursor 的读取、单调 checkpoint 和缺失 cursor baseline 放入私有 `event_cursors` 模块。该 cursor 仍独立于 event sequence、message ingress 和投影时钟。
5. 保持 rollback 的双时钟契约：event sequence 只由 append-only rollback marker 移动；projection cutoff 仍按规范化 `last_msg_at` 截断 messages、steps 与 usage。rollback marker、projection 删除、usage compensation、aggregate rebuild 及可选 replacement transcript继续处于同一 SQLite transaction，提交后才使 cache 失效并广播。
6. 保持单一 `SessionStore` facade 和共享 append/transaction helpers；不新增 backing database 入口、恢复源或公开子模块。

## 替代方案

- 在 rollback 时删除 event history：拒绝。历史由 append-only event cursor 和 rollback marker保留，active replay 解释当前分支。
- 分两笔事务写 usage event 与 usage projection/compensation：拒绝。event replay 会重新应用未被对应 discarded event 抵消的 usage。
- 将 Memory consumer cursor 与 session event sequence 合并：拒绝。二者表达不同的恢复进度，合并会丢失独立 checkpoint 契约。
- 将 branch/recovery marker 另建 store：拒绝。它们仍是同一 session event stream 的控制事件。

## 影响与恢复

schema、event payload、rollback 规则、usage 行与 UI/IPC 行为均未变化，不需要数据库重置或迁移。回退时恢复原私有模块布局即可。

## 验收

- `cargo test --workspace --locked` 通过；既有 stale rollback cursor、rollback/usage 原子补偿、projection cutoff、recovery marker、branch metadata、pending cursor 和 append-only replay 测试通过。
- `cargo clippy --workspace --locked -- -D warnings` 通过。
- `cargo fmt --all -- --check` 通过。
- ADR index 检查通过：1289 个唯一编号，所有本地链接有效。
