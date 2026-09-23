# ADR 0248：后台 action 终态提交与运行态发布顺序

- 状态：审查结论（实现延期，2026-09-24）
- 范围：`haven-tools::ActionService` 后台 action 终态、`haven-memory` CAS/outbox 边界
- 关联：[ADR 0180](0180-action-completion-outbox-and-stream-overflow-pump.md)、[ADR 0236](0236-background-action-terminal-cas.md)

## 背景与审查结论

数据库层已用 `running` 条件更新实现 first-wins CAS；完成/失败状态与 completion outbox 在一个 SQLite 事务内提交，取消使用独立 CAS 且不创建 outbox（ADR 0236）。这个事务保证了数据库内部一致，但当前 `ActionService` 没有用它的结果决定内存终态和通知。

审查确认以下问题：

1. `mark_finished`（`crates/tools/src/action_service.rs`，约 1663 行）和 `mark_cancelled`（约 1731 行）先把 `ActionEntry.state` 改为终态，再调用 `notify_completion`。
2. `persist_terminal`（约 882 行）只对数据库错误记录 warning；数据库闭包返回的 CAS `bool` 被丢弃，`Ok(false)` 与提交成功无法区分。
3. `notify_completion`（约 961 行）在持久化尝试之后无条件发 `action:finished` 和 transient `ActionCompletion`。因此数据库错误或 CAS 未获胜时，UI/Agent 仍可能看到一个数据库没有接受的终态。后台 action 没有 scheduled action 那种持有候选状态并重试持久化的 worker。
4. `cancel_owned_background_by_session`（约 1614 行）先从 `actions` 移除条目，再发送 kill 并尝试持久化取消。取消写入失败时，当前进程已经没有该条目，数据库仍可能保持 `running`；重启会把它改成 restart failure。
5. `attach_session` 可在 action 完成后才写 owner。现有 `update_action_session` 已在同一事务内更新 action 行与未投递 outbox 的 owner，并清除 claim lease；问题在于 ActionService 忽略这次持久化失败，并在终态已提交后再次调用 `notify_completion`，导致重复 transient completion。Agent 对无 owner 的 completion 不投递到 session 也不 ack；迟到绑定仍需要与内存仲裁和发布去重闭合。

重启时，仍为 `running` 的 action 会被 `mark_interrupted_actions` 标为 `failed`；completion outbox reconciliation 随后会从终态 action 行补建 outbox。稳定的 `action_result_id` / transcript `message_id` 能抑制重复投影，但不能让原先发出的内存终态、当前 action history 和重启后恢复出的终态变成同一事实。

## 决定

本次只记录审查，不改写单个通知函数或增加仅覆盖完成路径的 retry。修复必须在一个后续完整切片中统一处理以下边界：

1. 后台完成、失败、取消、session cleanup、late attach 与删除共用终态仲裁；数据库 CAS 的胜出结果是是否应用内存终态、发布 `action:finished` 和发 transient completion 的唯一依据。`Ok(false)` 必须与数据库错误分别处理，并从权威 action 行协调状态。
2. 完成/失败仍由 action 行更新与 outbox 插入的同一事务原子提交；只有 CAS 胜者可创建唯一 outbox。取消继续不创建 transcript completion outbox，Agent 仍跳过取消结果。
3. DB 暂时失败时必须保留完整候选终态（含完成输出/错误、时间戳和 owner），可在 worker 已退出后重试；CAS 成功前不得把它作为已提交终态发布。关闭进程时若仍无法提交，重启恢复仍按 durable `running` 行处理。
4. session cleanup 不得在取消 CAS 前丢弃唯一内存条目；数据库失败时必须保留可恢复/可重试的取消状态。进程 kill 与终态确认是两个不同动作。
5. late attach 必须让 action owner 与尚未 delivered 的 outbox owner 可恢复地一致。现有 `haven-memory::update_action_session` 已提供这条事务化写入边界并处理已 claim 但尚未 ack 的行；后续切片要让 ActionService 正确处理该操作的错误，并避免因已提交终态重复发布 transient completion。
6. 保持已存在的 transcript 投影后 ack、稳定 action result identity、取消不进 durable outbox、重启 recovery 与 UI cleanup 语义。

## 下一步实现与验收

下一切片应允许写入 `crates/memory` 对应 action/outbox repository，并视需要修改 Agent 的 session binding 调用点。按以下顺序实施：

1. 为终态存储结果建立显式结果类型（CAS 获胜、已被其他终态取代、存储错误），并提供按 action id 读取权威终态/owner 的路径。
2. 在 ActionService 建立单一后台终态仲裁与可重试终态候选；先完成 durable transition，再变更内存投影和发布事件。清理、late attach、删除也经过同一协调边界。
3. 复用并补强现有 late-attach 存储事务：保留已 delivered outbox 不变，覆盖已 claim、未 ack 的行；ActionService 只有在 owner 持久化成功后才发布对应的绑定/补发信号。
4. 用 SQLite trigger 注入完成/取消/outbox 错误，验证失败时不发布终态、状态/候选可重试；恢复 DB 后只有一个 CAS 获胜者、一个 outbox 和一次 transcript 投影。
5. 用并发屏障覆盖完成对取消、完成对 session cleanup、late attach 对完成、删除对终态提交；再验证 shutdown/restart 后 running recovery 与 outbox reconcile 不丢不重。

本次风险较高的原因是 action_service-only 的改法无法原子修复 owner/outbox 快照，也无法安全地把 CAS 输家从内存/UI/Agent 通道中撤回。只增加 warning 检查或照搬 scheduled retry 会留下部分路径仍然先发布、部分路径等待持久化的双重语义。

## 验证与回滚

本 ADR 为审查记录，没有代码或测试变更。本次结论依据 `action_service.rs`、对应测试、`ActionCompletionOutbox`/action repository 与 Agent completion consumer 的现有实现。

后续实现需按项目门禁运行格式化、`cargo test --locked -p haven-tools`、受影响的 `haven-memory`/`haven-agent` 测试、workspace strict Clippy；由于会跨 crate，另跑 `cargo test --workspace --locked`。无 schema 变更时无需数据库重置；若实现改变 outbox schema，则必须另立契约变更与重置说明。
