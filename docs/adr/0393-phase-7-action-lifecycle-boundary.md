# ADR 0393：阶段 7 Action 生命周期与终态投影边界

- 状态：已采纳（2026-09-29）
- 范围：收口阶段 7 中 background/scheduled 的 trigger 与 execution owner、终态副作用及 scheduled tool outcome 投影
- 关联：ADR 0317、0325、0332、0334、0338、0343、0344、0352、0373、0375、0392

## 背景

ADR 0352、0344 与 0373 已记录可共享的状态判断、transport/UI mapper，以及两类 action 的差异。ADR 0373 采纳了 background 与 scheduled completion 记录、任务卡和 transcript 的统一投影格式；ADR 0392 完成了 dependency-waiting 的 durable relation 与 restart recovery。本 ADR 明确共享投影如何复用既有 outbox 与 X12 transcript owner，并关闭“是否抽出完整通用 Job lifecycle”的阶段 7 歧义。

## 决定

1. 保留各自的 execution owner。`ActionService` 负责 background 的 admission、子进程启动/等待/取消与 terminal persistence；它也负责 scheduled 的 At/After/dependency trigger、durable fire CAS 与 fire publication。`AgentLayer` 负责执行 scheduled tool 和 Continue，并回报 scheduled terminal。trigger policy 与 execution owner 的分离是这些既有职责边界，不要求把 background 与 scheduled 包进共同 executor。
2. 共享语义相同的策略与 transport：Action status graph、terminal eligibility、`ActionLease<T>` 的 lease 判断、terminal persistence retry policy、`ActionStore` 与 `ActionCompletion` transport。各 kind 的 durable CAS/outbox、claim identity/时钟、rollback 和 execution side effects 仍由原 owner 管理。`ScheduledTriggerRequest` 是 scheduled 专用的 At/After policy，不是 background immediate admission 的抽象；output tail 只属于 background/foreground shell，scheduled 不新增 tail。
3. Completed/failed 的 scheduled tool 结果在 owner session 存在时复用既有 `action_completion_outbox` 与 Agent action-result consumer。`ActionService` 原子提交 terminal row 和 outbox；有界成功摘要或失败原因只存于 ADR 0392 的 `actions.result_summary`，并同时供 dependency continuation 与 transcript result 使用。稳定 `action_result_id` 使用 action id，Agent 与 background 共用不可信 JSON result envelope、`ActionResult` 队列及 X12 投影：live session 由 actor 处理，terminal session 走 `persist_session_message`，投影成功后 ack。相同消息 ID 保证重复投影幂等；原始工具 output 不写日志或新增 UI event。
4. Scheduled Continue 继续使用既有 session input / Agent conversation 路径，不产生第二条 terminal result。Cancelled scheduled action 不创建 completion outbox 或 result transcript；unowned、已删除 session 与重复投影遵循既有 action-result ack/idempotency 行为。Scheduled completion 的既有任务卡与通知契约保持不变。
5. 阶段 7 不继续抽取完整通用 Job 状态机或 executor。action-level execution timeout、独立 owner token、lease renewal、automatic replay、新 action status/kind，以及独立于既有 `action_result_id` 的通用 terminal event identity 均不属于本阶段契约；background 与 scheduled result 复用稳定 action result ID 和幂等投影。既有各类工具自身的 timeout/retry 契约不变。

## 影响

ADR 0352 保留为生命周期审计记录；其中 dependency relation/recovery 的旧快照由 ADR 0392 更新，完整 Job 抽象开放问题由本决定关闭为非目标。ADR 0344 的投影边界由本决定明确为 kind-specific execution side effects 加共享 terminal result contract。ADR 0373 的统一 terminal transcript 决策不被收窄：scheduled tool 的有界结果沿用 background action-result 投影路径，Continue 则保留既有输入 transcript。ADR 0392 将 `actions.dependency_result` 收敛为单一 `actions.result_summary`，schema 升至 v30；旧数据库继续按 reset contract 重建。

阶段 7 的 MemoryRuntime ownership、已采纳的 Action 局部策略、ADR 0392 的 dependency recovery 与本 ADR 规定的跨 kind transcript result 投影一并构成该阶段的明确完成边界。此次实现不改变 IPC、状态集合或引入新的 UI event；schema 变更受 v30 reset contract 管理。

## 验证与回滚

实现回归覆盖 terminal row/outbox atomicity 与 restart reconciliation、scheduled completed/failed transport、unowned/cancelled/Continue 分支、稳定 result identity、重复 transcript projection 与不可信 envelope。`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked` 均通过。未来若改变 execution owner 或跨 kind terminal result 语义，先更新本 ADR 和相应 kind 的产品副作用契约，再单独实现并验证；schema v30 的回滚仍按 `docs/release-and-reset.md` 执行。
