# ADR 0352：Background/Scheduled action 生命周期转换内核审计

- 状态：已采纳（2026-09-25）
- 范围：Phase 7 的 background/scheduled action trigger、admission、claim、execution、observation、terminal、retry、cancel 与 cleanup
- 关联：[ADR 0259](0259-memory-runtime-committed-event-consumer.md)、[ADR 0305](0305-action-service-action-store-port.md)、[ADR 0317](0317-action-terminal-lifecycle-kernel.md)、[ADR 0321](0321-action-owned-session-cancellation-traversal.md)、[ADR 0325](0325-action-completion-transport-ownership.md)、[ADR 0332](0332-action-claim-lease-core.md)、[ADR 0334](0334-action-terminal-persistence-retry-policy.md)、[ADR 0338](0338-action-tail-output-policy-and-snapshot.md)、[ADR 0343](0343-action-trigger-policy-boundary.md)、[ADR 0344](0344-action-lifecycle-ui-projection-boundary.md)
- 后续：dependency-waiting 的持久化与重启恢复见 [ADR 0392](0392-durable-scheduled-dependency-recovery.md)；阶段 7 的 Action owner 与终态投影边界见 [ADR 0393](0393-phase-7-action-lifecycle-boundary.md)。

## 审计结论

当前没有适合继续抽取的、覆盖完整 background/scheduled Job 生命周期的纯 `ActionLifecycleTransition` 内核。可安全共享的纯判断已经有单一 owner：`ActionStatus::can_transition_to` 定义通用 status graph；`action_terminal::can_claim_terminal` 定义终态来源/目标准入；`ActionLease<T>` 定义同一时钟域内的 claim 有效期；`ActionPersistenceRetryPolicy` 定义终态持久化修复退避；`ScheduledTriggerRequest` / `ScheduledTriggerCandidate` 定义 scheduled trigger 输入归一化和 due-time policy。按 session 选取 live action 的共同遍历骨架也已由 ADR 0321 收口。

审计未发现 background 与 scheduled 重复实现同一状态转换判定。background admission 直接以 `running` 登记并启动进程，没有 `waiting → running` trigger；该转换只存在于 scheduled fire。两条路径调用相同的 `can_claim_terminal`，durable 条件更新仍按 kind 使用各自的 CAS。持久化前后或等待期间重复检查当前状态，是对不同竞态/提交边界的重新校验，不是可合并的重复 policy。若再加一层全生命周期 policy，只会包装已有 helper，或把 family-specific CAS、rollback 与副作用误建模为纯状态转换。

## 当前 lifecycle owner

| 阶段 | Background | Scheduled | 已共享的纯策略 / 保持分开的原因 |
|---|---|---|---|
| Trigger 与 admission | `ShellTool` 调 `ActionService::spawn_shell_for_session`；校验命令、容量与 shutdown，先保存 `running` row，再登记内存状态、建立 process containment 并启动 child。启动失败走 registration rollback；`action:created` 在进程成功 admit 后发布。 | `ScheduledActionTool` 做输入前置校验；`ActionService::set` 调 scheduled trigger policy、校验 payload/容量，持久化可恢复 schedule，登记 `waiting`，发布 created 后装配 timer 或进程内 watcher。 | Scheduled 的 `At`/`After` 归一化已纯化；background immediate admission 仍包含持久化、进程创建和失败补偿，没有共用的无副作用转换。watch dependency 不落库。 |
| Claim | child execution 没有第二个 ActionService claim；terminal row CAS 见下方。AgentLayer receiver 通过 ActionService/ActionStore 从 durable completion outbox 以 `action_result_id` 和 30 秒 lease claim。取消不创建 durable completion outbox。 | 定时 fire 先对 durable row 做 `waiting → running` CAS（watch dependency 是进程内分支），再更新内存并发布 `ScheduledActionFired`。receiver 通过共享 pending-fire map、`action_id` 和 15 分钟进程内 lease claim。 | `ActionLease<T>` 共享纯 lease 判断；SQL transaction、fire CAS、两类 identity 与迟到 consumer 恢复由各自 owner 保持。 |
| Execution | `ActionService` 启动/等待/kill child，并 drain stdout/stderr；失败或取消不会自动重启 child。 | AgentLayer 消费 fire 后经 tool runner 执行 tool mode，或向关联 session 提交 continue prompt；AgentLayer 回报 scheduled terminal。scheduled execution failure 不自动重跑。 | 两类工作分别是 OS child process 与 Agent tool/session 调用，不是同一执行器或可互换的纯转换。 |
| Observation 与 delivery | `ActionOutputPort` 提供 bounded live tail 和 `action:output`。完成结果由 AgentLayer 投影到 session transcript，durable projection 成功后才 ack outbox。 | running `action:updated` 与 terminal `action:finished` 由 ActionService 发布；AgentLayer 负责通知及会话效果。scheduled 没有 output tail。 | 完成消息、session projection 与 UI lifecycle event 有不同消费者、ack 及可见行为，不能合成一个 observation reducer。 |
| Terminal | `action_terminal` 构造状态和时间戳；durable running→terminal CAS 对 completed/failed 同事务写 outbox；CAS 胜者才更新内存并发布，输家对齐 durable row 且不通知。cancelled 不写 outbox。 | 共用 terminal constructor/source predicate/guard；持久 schedule 做 running→terminal CAS，durable commit 后才更新内存和发布。watch dependency 保留原进程内语义。waiting schedule 只允许直接取消。 | 终态候选准入已共用；background outbox 和 scheduled row 的事务与 publication 顺序不同，必须留在 family owner。 |
| Retry | terminal write failure 由 `ActionPersistenceRetryPolicy` 修复，不重跑 background process；Agent completion delivery retry 只保证 transcript projection 后 ack。 | terminal repair 共用同一 policy，但每次 scheduled store 调用仍先做原有短重试；fire 无 consumer 时尝试 `running → waiting` 并 re-arm，rollback 失败时保留 pending fire。 | 终态持久化退避 decision 已共享；具体 store attempt、Agent delivery retry 与 timer rollback 语义不同。 |
| Cancel | `ActionService::cancel` 对 Running background 只发送 process kill，child runner 随后尝试提交 cancelled；session cleanup 的 shared selector 不改变该回调语义。 | `cancel_scheduled` 可从 Waiting/Running 进入 cancelled，持久化成功后才改内存并发布；失败时 live state/timer 保留。session cleanup 用 shared selection/serial traversal，再调用 scheduled callback。 | session-owned live-action 选择/遍历已共用，kind-specific cancel signal、CAS 与错误路径不等价。 |
| Shutdown、cleanup 与恢复 | shutdown/session cleanup 取消运行中的 child；session cleanup 会移除对应 terminal board entry。terminal board entry 按 TTL 清理；restart cleanup 将遗留的 durable running row 标为 failed，不重启 child。 | shutdown 取消已 running 的 fire，但保留 waiting durable schedule。审计发现 `ActionService::set` 的 admission 清理条件曾移除 Running 和 terminal entries；ADR 0353 已将它收窄为只清理 terminal entry，保证 Agent terminal callback 仍能找到 Running row。restart 只恢复 durable waiting schedules；遗留 running scheduled row 由 restart cleanup 标 failed，不 replay；watch dependency 是进程内关系，不跨重启恢复。 | terminal 保留/删除是两个 board 生命周期。scheduled admission cleanup 的历史偏差与修复见 ADR 0353；没有新增 owner token、续租、execution timeout、自动 replay 或跨重启 dependency watcher。 |
| UI projection | `actionStore` 暂存 finished payload 供绑定的 background tool card 读取，layout 复用 `finalizeBackgroundActionMessages`；仅 background 有 live tail。AgentLayer 的 durable session transcript projection 仍是另一条 completion path。 | finished 从 live board 移除，由 Agent 通知路径处理；scheduled 不重写 background tool card，也没有 live tail。 | DTO mapper、live-row predicate 和 upsert 已共用；finished handler 的副作用不同，且没有 durable UI event identity，故不加跨 kind reducer/dedup。 |

## 决定与剩余决策

本切片只增加 exhaustive status/terminal claim table tests 并记录审计，不新增 runtime policy 或状态机，也不改 action kind/status、CAS/outbox/lease、cancel、tail、terminal event order、错误语义、IPC、DB、ID 或 UI 行为。MemoryRuntime/MemoryWorker 的 committed-event 与 durable outbox 属于 ADR 0259 的独立 lifecycle，不并入 ActionService Job。

本审计发现一个非抽取类 cleanup 风险：scheduled admission 的 `retain` 谓词曾从内存 registry 删除 Running scheduled action，尽管代码注释称只回收 terminal entries。ADR 0353 在独立修复切片添加 admission、in-flight completion、取消、no-consumer recovery 与 restart 回归，并将清理条件限定为 terminal entries；此修复没有抽取新的生命周期 policy。

如后续仍要统一完整 Job lifecycle，先决定：是否把 immediate background trigger 与 process runner 分离；`running` 是否要区分已触发、已交付和已开始执行；scheduled watcher 是否需要跨重启及其 producer identity/idempotency/cursor；execution deadline、claimant owner token/续租与迟到执行之间的副作用/恢复契约；以及跨 kind UI terminal event 的稳定 identity 与消费语义。在这些决策之前，禁止添加 action-level timeout、owner token/lease renewal、自动 replay、跨重启 dependency watcher 或新 Job 状态语义。

## 验证

- 新增测试穷举现有五个 ActionStatus 的 transition graph 与全部 terminal target/source 组合，固定只允许 Waiting→Running/Cancelled、Running→terminal，以及相同 status 的幂等判定；terminal claim 仅允许 Running 的终态提交和 Live 的 Waiting→Cancelled。
- 保留 `ActionService` 测试对 durable CAS、outbox、no-consumer rollback、retry、cancel、恢复、事件顺序和错误路径的覆盖；Agent 测试保留 background transcript durable ack 与 scheduled fire/outcome 覆盖。
- 运行 `cargo fmt --all`、`cargo test --locked -p haven-tools`、`cargo test --locked -p haven-agent`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`。
- 不改 UI，因此不运行 UI 门禁。

没有 schema、配置、IPC、ID 或用户数据迁移。
