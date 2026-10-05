# ADR 0521：Session confirmation 决议与恢复唤醒原子提交

## 状态

已完成（2026-10-06）。

## 背景

SessionActor 当前先追加 `interaction_resolved`，提交成功后把 actor 内请求标为 resolved。若这是最后一个 pending confirmation，`SessionSupervisor::resolve_interaction` 随后再单独把持久 session 状态从 Paused 改为 Pending，之后才发布 `SessionResumed`、入队并唤醒 dispatcher。

因此第二次持久写入失败会留下已终结的 durable interaction 与仍为 Paused 的 session；actor 不再持有 pending request，调用却返回错误，且没有 resume event、queue admission 或 dispatcher wake。同 request retry 会得到 stale，无法通过确认重试恢复。这与 ADR 0424「决定持久化与唤醒不能分离成事件已写但 session 仍暂停」的 lifecycle invariant 不符。

SessionStore 已提供 `append_domain_event_batch_with_session_status`，在一个 SQLite transaction 中 compare-and-set session status、追加 domain events，并只在 commit 后广播。确认请求批次目前已使用该边界；ADR 0519 只覆盖 grant 成功后 `interaction_resolved` append 自身失败，尚未覆盖 resolved event 成功后 status transition 失败的窗口。

步骤 0 复核把它选为 ADR 0520 后唯一 Next：这是已定义契约与实现顺序之间的具体差异，且能用存储故障注入验证；不需要泛化重构 `session_events.rs`。

## 目标决定

1. 当一个 Session confirmation 的 resolve/expiry 决议会结束最后一个 pending confirmation，且 actor 当前状态为 Paused 时，由 SessionActor 在一个 SessionStore transaction 中追加 `interaction_resolved` 并将持久状态 compare-and-set 为 Pending。
2. 只有该 transaction 成功后，actor 才更新 interaction registry、`SessionInfo` 和 status watch。失败时 event 与 status 都保持原值，request 继续 pending，session 继续 Paused，可使用同 request 重试。
3. 其他 confirmation 仍 pending 时，只追加 resolved event，不提前恢复 session。最后一项完成后再进行上面的原子写入。
4. SessionSupervisor 保留 post-commit lifecycle/UI owner：仅在 actor 返回“本次事务已完成 Paused→Pending”后发布一次 `SessionResumed`、enqueue 并 wake；删除第二次独立 status 写入。若 actor 当前不是 Paused，不伪造恢复事件。
5. `grant_session_permission` 仍按 ADR 0519 先持久化 grant 再进入 resolve；grant 写与 event/status transaction 之间不承诺原子性。本 ADR 只收口决议 event 与 session resume status。

## 非目标与影响

- 不修改 event payload、schema、IPC、SessionStore transaction owner 或其他 interaction owners；不迁移/重置数据库。
- 不在某个 confirmation 解决时提前 enqueue；同批其他 confirmation 仍 pending 时 session 保持 Paused。
- 不把 Actor 状态写入 projection/snapshot，也不引入补偿删除或以 stale retry 推断 durable outcome。
- ADR 0424 的 owner 路由、deadline 和 first-wins 规则不变；ADR 0402/0519 的授权 grant 行为不变。

## 验收与停止条件

在内存 SQLite 中建立带一个 pending confirmation、状态为 Paused 的 session，并订阅 dispatcher wake。创建临时 trigger 拒绝 Paused→Pending status update，调用确认决议并断言：

- 调用返回可重试错误；event log 无对应 `interaction_resolved`；actor 仍报告原 request pending，actor 与 DB session 均为 Paused；没有 `SessionResumed`、queue admission 或 dispatcher wake。
- 移除 trigger 后以同 request ID 重试成功；只提交一条 resolved event，actor 与 DB 都转为 resolved/Pending；只发一次 `SessionResumed` 并 enqueue/wake 一次，pending session 可被 dispatcher claim。
- 多个 pending confirmations 时，前序决议只追加事件、不会改 status/wake；最后一项走相同原子提交路径。

若存储 API 无法在一次 transaction 中表达现有 first-wins/status CAS，或注入证明此分离窗口不可达且 stale retry 有明确、可见、可重试的产品语义，暂停实现并修订本 ADR；不扩展为通用 event store 拆分。

跨 Agent/SessionStore durable lifecycle 边界，完成时运行 `cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、crate dependency inventory、ADR index 与 `git diff --check`。

## 实施结果（2026-10-06）

- 最后一个 pending confirmation 在 Paused session 上 resolve 或 expire 时，SessionActor 通过 `append_domain_event_batch_with_session_status` 在同一事务写入 `interaction_resolved` 并 CAS 为 Pending；提交成功后才更新 actor registry、`SessionInfo` 与 status watch。仍有其他 pending confirmation 时只追加决议事件，不提前恢复。
- SessionSupervisor 删除了第二次 status 写入，只在 actor 确认原子转换成功且 session 仍可恢复时发布 `SessionResumed`、入队并唤醒 dispatcher。
- 故障注入回归覆盖两个 pending confirmation：拒绝 Paused→Pending 后，event、actor/DB status、pending request 和 wake 均保持原状；去掉 trigger 后同 request 重试成功，单次恢复/唤醒且可 claim 一次。另有 expiry 路径回归，确认过期的最后一项也原子恢复并只唤醒一次。
- 适用门禁通过：`cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、crate dependency inventory（11 crates / 30 directed edges）、ADR index（504 records）与 `git diff --check`。
- 本轮步骤 0 复核没有发现达到准入条件的下一个结构切片；pending-session 批次读取重试、`session_events.rs`、ActionService、crate/API 边界与性能候选继续 Deferred，按路线图 §5.5 的触发证据再评估。Windows 发布验收仍是独立 Open Gate。

## 回滚

回滚实现与回归测试，恢复 ADR 0520 前的分离 resolve/status 路径并将本记录从路线图已完成移回 Deferred。无 schema、IPC 或用户数据变化，无需迁移/重置；回滚重新引入 ADR 0424 已禁止的 event/status 分离窗口。
