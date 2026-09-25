# ADR 0334：Action 终态持久化重试策略归纯 typed owner

- 状态：Implemented
- 日期：2026-09-25
- 范围：`haven-tools::ActionService` background/scheduled 终态持久化修复重试
- 关联：[ADR 0305](0305-action-service-action-store-port.md)、[ADR 0317](0317-action-terminal-lifecycle-kernel.md)、[ADR 0321](0321-action-owned-session-cancellation-traversal.md)、[ADR 0325](0325-action-completion-transport-ownership.md)、[ADR 0332](0332-action-claim-lease-core.md)

## 背景与现有语义

`ActionService` 原本在 background 与 scheduled 两个终态持久化修复 worker 中分别维护相同的退避循环：初次持久化失败后等待 1 秒，后续失败将间隔翻倍并封顶 30 秒，直到写入成功、发现终态仲裁已由别处赢得，或应用 shutdown。两条路径的数据库事务和终态发布仍不同：background 用一个数据库尝试提交 terminal row 与 completion outbox；scheduled 的每次持久化调用先保留最多 3 次、间隔 50 ms 的短重试，且不写 background outbox。

本切片处理的是 **ActionService 的终态持久化修复重试**，不是再次运行 command/tool/session。现有 background shell 没有 action-level 执行 deadline；scheduled 的 `due_at` 是触发时刻，不是执行超时。scheduled fire 的 tool 调用或 Continue 模式启动的 Agent run 不由 ActionService 重放。

此外，AgentLayer 对 background completion 的 transcript 投影/入队失败仍按 100 ms 重试，并在 durable transcript projection 后按 `action_result_id` ack outbox。它属于 delivery/projection，不是终态持久化或 action job retry。provider/LLM 请求 retry 仍由 LLM/Agent 请求路径拥有；ReAct tool-call retry 仍由 Agent 工具批次策略拥有。本决定不改动这些策略。

## 决定

1. 新增 crate-private `ActionPersistenceRetryPolicy` 与 `RetryDecision`，作为纯函数策略 owner。它只接收当前 monotonic `Instant`、已完成的策略尝试数与 typed `RetrySignal`，返回下一尝试号和退避，或 typed `RetryStopReason`。它不读取 clock、sleep、访问 `ActionStore`、调用 LLM 或拥有终态转换。
2. background 与 scheduled 的持久化修复 worker 都使用同一策略输入：当前首次终态持久化已失败，修复 worker 的最大尝试数为 unlimited，retry deadline 为 `None`。因此现有 1 秒起始、指数增长、30 秒封顶且不主动超时的行为保持不变。策略在真正开始一次已排程 retry 前重新检查显式 deadline；目前两条生产路径都不配置 deadline。
3. `Failure { retryable: true }` 对应现有所有 `Err` 持久化结果（保持 retry-all 行为）；`Terminal` 对应成功的 CAS 输家或另一终态已胜出，`Succeeded` 对应 durable commit 成功，`Cancelled` 对应 shutdown 取消。其它 typed stop reason 仅表明 retry 决策停止，不改写 action 状态、错误文本或 `error_reason`。
4. family-specific 编排保持原 owner：background 继续经原 CAS 原子提交 terminal row 与 outbox，提交成功后才更新内存状态并发布；scheduled 继续经原 `Running → terminal` CAS，durable 成功后才完成内存状态与事件。原来的 scheduled 每次 store operation 内部短重试、terminal guard、claim/lease 清理、outbox ack、scheduled no-consumer timer rollback 和 shutdown 顺序均保持。
5. 不把 outbox claim lease、scheduled fire lease、fire recovery 或 AgentLayer durable completion delivery retry 放进本策略。background 30 秒 durable outbox lease、scheduled 15 分钟进程内 lease、稳定 identity、ack 时序与 CAS arbitration 均不变。

## 替代方案

- 将 ActionStore、CAS/outbox、timer 或 Agent completion delivery 搬进策略 owner：会把持久化/副作用放入纯策略，或跨越 Tools/Agent 所有权，拒绝。
- 把 scheduled 的 3 次、50 ms 短 store retry 与 terminal repair backoff 视为同一个 attempt 序列：会改变 attempts 和延迟的含义，拒绝。
- 把背景 shell 的无 deadline 改成有限超时，或因 scheduled Continue/tool 失败自动重新执行：会改变 job 行为并可能重放副作用，拒绝。
- 复用 provider/LLM 请求 retry 作为 action job retry：二者的幂等性、terminal/outbox 事务与所有权不同，拒绝。

## 影响与验证

- 无 schema、ID、IPC、错误文本、action status 或用户数据变化。
- 策略单测覆盖无 deadline/预算、deadline 到期、不可重试失败、预算耗尽、cancel/terminal/success 优先级、下一 attempt 计数与 `1, 2, 4, 8, 16, 30` 秒封顶序列。
- 既有 ActionService 回归测试继续覆盖 background terminal/outbox 写入失败后保持 running 并在提交后发布、background cancel 只在提交后发布，以及 scheduled terminal DB 失败时先保持 running、重试成功后再终态投影。
- 验收命令：`cargo fmt --all -- --check`、`cargo test --locked -p haven-tools`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`、`git diff --cached --check`。

## 未完成工作与回滚

这不是完整 Job 生命周期：trigger/execution 分离、真正的 action execution timeout、tail output 与统一 UI projection 仍待后续 Phase 7 切片。当前 retry deadline 为 `None`，不会增加执行超时。回滚时移除 `ActionPersistenceRetryPolicy` 并将两个修复 worker 恢复为原 1 秒起步、30 秒封顶循环，再回退本 ADR、路线图和架构记录；无需 schema 或用户数据重置。
