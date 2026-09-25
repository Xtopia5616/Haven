# ADR 0343：Scheduled action trigger policy boundary

- 状态：Implemented
- 日期：2026-09-25
- 范围：`haven-tools` scheduled action trigger admission；不定义新的 Job 状态机
- 关联：[ADR 0305](0305-action-service-action-store-port.md)、[ADR 0317](0317-action-terminal-lifecycle-kernel.md)、[ADR 0321](0321-action-owned-session-cancellation-traversal.md)、[ADR 0325](0325-action-completion-transport-ownership.md)、[ADR 0332](0332-action-claim-lease-core.md)、[ADR 0334](0334-action-terminal-persistence-retry-policy.md)、[ADR 0338](0338-action-tail-output-policy-and-snapshot.md)

## 审计结论

Action trigger 与实际执行并非由一个跨所有 kind 的 owner 承担，但 `ActionService` 同时是 board/runtime registry、后台 shell 执行 owner 和 scheduled lifecycle owner。当前调用边界如下：

| 职责 | 当前 owner 与顺序 |
|---|---|
| 创建与 admission | Background shell 由 `ShellTool` 调用 `ActionService::spawn_shell_for_session`；ActionService 先持久化 running row，再登记内存 board、启动 child process，成功后发 `action:created`；启动失败时按既有路径回滚 registration。Scheduled 由 `ScheduledActionTool` 做模型输入/工具可执行性前置校验，再调用 `ActionService::set`；ActionService 持久化 timer action 后登记 `Waiting`、发 `action:created` 并安装 timer 或 dependency watcher。 |
| Trigger | Background 是立即启动的 child process。Scheduled `due_at`/`delay_secs` 经 timer 触发；`watch_action_id` 经进程内 watcher 轮询 producer 状态触发。ActionService 在 scheduled fire 时先 durable `Waiting → Running` CAS，再更新内存状态、发 `action:updated` 并发布 completion。 |
| Claim/lease | Background completion claim 在 ActionStore/outbox 中以稳定 `action_result_id` 和 30 秒 lease 恢复，Agent transcript/event 投影 durable 后 ack。Scheduled fire claim 与 pending recovery map 在 `action_completion` 中，以 `action_id` 和 15 分钟进程内 lease 去重；没有跨进程恢复。两者共用纯 `ActionLease<T>` 判断，但 storage/recovery owner 不同。 |
| 执行 | Background child process、stdout/stderr drain、kill 和结果收集由 ActionService 执行。Scheduled fire 由 AgentLayer 消费：tool mode 经 tool runner 执行已存 builtin tool，continue mode 向关联 session 提交 prompt；必要的确认由 tool runner 处理。Agent 再调用 ActionService 的 scheduled terminal API。 |
| 终态与 retry | ActionService 通过既有 `TerminalTransitionGuard`、ActionStore CAS/outbox、进程内投影及 `ActionPersistenceRetryPolicy` 管两类 action 的 terminal commit/retry。Scheduled store 短重试、background outbox transaction 和 watch dependency 的进程内终态语义仍按原 family path 执行。 |
| Event/UI 与 Agent 投影 | ActionService 的 `ActionLifecycle` 发 `action:created/updated/output/finished`；App bootstrap 注册 sink，App adapter 投影为既有 `ActionEvent`/Tauri wire；`commands/action.rs` 读取 board/history。AgentLayer 另负责 background completion 的 transcript 投影与 outbox ack、scheduled execution outcome/session notification。这些不是同一投影。 |

结论：trigger 创建、运行时生命周期、终态和 lifecycle event 在 ActionService 中有意相邻；scheduled 的 actual tool/session execution、background result 的 transcript 投影、App IPC/UI mapping 各有独立 owner。没有一个全局 owner 同时负责 trigger、claim、provider/tool execution、终态和 UI projection。把这些职责整体移动或统一会跨越既有 CAS/outbox、Agent tool-runner 与 Tauri event 边界，当前不安全。

## 决定

新增 crate-private `action_trigger_policy`。`ScheduledTriggerRequest` 归一化 watch action id，并分类当前三类输入；`ScheduledTriggerCandidate` 以调用方一次提供的 UTC 时钟计算绝对时间或相对 delay，保留 `num_seconds()` 的未来边界与配置 horizon 校验输入；最终 typed trigger 只有 `At { due_at, remaining_secs }` 与 `AfterAction { action_id }`。

`ActionService::set` 仍负责读取 horizon 配置、校验并保存其余 action 约束、生成 `act-*`、ActionStore durable admission、pending capacity、board insertion、timer/watch worker、fire CAS、状态/event 顺序及失败处理。Background immediate trigger 仍由 `spawn_shell_for_session` 的直接调用表达，本 ADR 不创建共享 `Immediate/At/After` action schema。`ScheduledActionTool` 保留面向模型输入的前置校验与原错误文案；ActionService policy 保留其独立 admission 校验，避免降低 service 边界的保护。

保持不变：action kind/status、数据库 schema 和 CAS/outbox transaction、`action_result_id`/`action_id` identity、claim lease 时长及恢复、取消与 shutdown、terminal retry、tail snapshot、terminal event 顺序、ActionEvent/IPC、scheduled mode 执行及 Agent transcript/ack 语义。没有 owner token、lease renewal、action-level timeout、Job lifecycle 或 UI projection 新模型。

## 验证

- Policy 单测覆盖 relative delay、absolute due UTC 归一化、配置 horizon 边界、watch id trim、互斥和缺省输入、延迟范围、非法时间、未来秒数取整边界及既有错误文本。
- `ActionService` 现有 tests 覆盖 scheduled durable admission、fire CAS/event 次序、timer recovery/no-consumer rollback、terminal retry 与 cancellation；Agent tests 覆盖 background durable completion 投影/ack 和 scheduled fire dispatch/terminal 回报。
- 验收：`cargo fmt --all -- --check`、`cargo test --locked -p haven-tools`、`cargo test --locked -p haven-agent`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`。
- 无 schema、ID、IPC 或用户数据迁移。

## 未决点与替代方案

- `Immediate` trigger 是否需要成为显式、可持久化的内部类型，以及 background process runner 是否应从 ActionService 分离：需要同时定义 admission/启动失败窗口、进程取消、恢复和 terminal ownership，留待后续完整调用链设计。
- Scheduled `At`/`After` trigger 是否需要共同的持久表示，以及 dependency watcher 是否需要跨重启：后者需要 producer identity/idempotency 和可恢复 cursor；目前 watcher 是进程内语义。
- 是否统一 Action UI lifecycle projection 与 Agent completion/session projection：消费者、重试、敏感字段和 durable acknowledgement 不同，需先定各自权威和重放契约。
- 是否增加 action execution deadline 或 claimant owner token/续租：需要定义副作用重放、过期 claimant 与 shutdown 竞态，当前均不引入。

把 due 计算继续留在 ActionService 可避免新模块，但会让 trigger 输入策略和异步 admission 生命周期继续耦合在同一实现。将完整 trigger/execution state machine 移出 ActionService 则需要更改多个既有 owner 与故障边界，超出窄切片。因此本 ADR 只抽取无副作用、易回归的 scheduled trigger policy。

## 回滚

移除 `action_trigger_policy` 模块及本 ADR/架构路线图记录，把 typed policy 的同一分支恢复到 `ActionService::set`。没有数据库、配置、IPC 或用户数据回滚。
