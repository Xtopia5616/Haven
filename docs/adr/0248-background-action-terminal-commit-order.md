# ADR 0248：后台 action 终态提交与运行态发布顺序

- 状态：已实施（2026-09-24）
- 范围：`haven-tools::ActionService` 后台 action 终态、`haven-memory` CAS/outbox 边界
- 关联：[ADR 0180](0180-action-completion-outbox-and-stream-overflow-pump.md)、[ADR 0236](0236-background-action-terminal-cas.md)

## 背景与审查结论

数据库层已用 `running` 条件更新实现 first-wins CAS；完成/失败状态与 completion outbox 在一个 SQLite 事务内提交，取消使用独立 CAS 且不创建 outbox（ADR 0236）。这个事务保证了数据库内部一致，但当前 `ActionService` 没有用它的结果决定内存终态和通知。

实现前审查确认以下问题：

1. `mark_finished`（`crates/tools/src/action_service.rs`，约 1663 行）和 `mark_cancelled`（约 1731 行）先把 `ActionEntry.state` 改为终态，再调用 `notify_completion`。
2. `persist_terminal`（约 882 行）只对数据库错误记录 warning；数据库闭包返回的 CAS `bool` 被丢弃，`Ok(false)` 与提交成功无法区分。
3. `notify_completion`（约 961 行）在持久化尝试之后无条件发 `action:finished` 和 transient `ActionCompletion`。因此数据库错误或 CAS 未获胜时，UI/Agent 仍可能看到一个数据库没有接受的终态。后台 action 没有 scheduled action 那种持有候选状态并重试持久化的 worker。
4. `cancel_owned_background_by_session`（约 1614 行）先从 `actions` 移除条目，再发送 kill 并尝试持久化取消。取消写入失败时，当前进程已经没有该条目，数据库仍可能保持 `running`；重启会把它改成 restart failure。
5. `attach_session` 可在 action 完成后才写 owner。现有 `update_action_session` 已在同一事务内更新 action 行与未投递 outbox 的 owner，并清除 claim lease；问题在于 ActionService 忽略这次持久化失败，并在终态已提交后再次调用 `notify_completion`，导致重复 transient completion。Agent 对无 owner 的 completion 不投递到 session 也不 ack；迟到绑定仍需要与内存仲裁和发布去重闭合。

重启时，仍为 `running` 的 action 会被 `mark_interrupted_actions` 标为 `failed`；completion outbox reconciliation 随后会从终态 action 行补建 outbox。稳定的 `action_result_id` / transcript `message_id` 能抑制重复投影，但不能让原先发出的内存终态、当前 action history 和重启后恢复出的终态变成同一事实。

## 决定与实施

后台完成、失败、取消与 session cleanup 现在共用 `ActionService::try_commit_background_terminal`。`terminal_gate` 串行化本进程内的终态尝试；数据库的 `running` 条件 CAS 继续作为跨连接/跨实例的权威仲裁。

1. `persist_terminal` 保留三种结果：`Ok(true)` 为提交成功，`Ok(false)` 为 CAS 输家，`Err` 为存储错误。只有 `Ok(true)` 才更新内存终态、发 `action:finished` 和 transient completion。无数据库的 headless 模式使用同一内存 first-wins 仲裁。
2. CAS 输家通过 `get_action` 将运行态投影对齐到数据库终态，不发布任何终态通知。完成/失败仍由现有 SQLite 事务原子提交 action 与 outbox；取消仍不创建 transcript outbox。
3. 存储错误时内存保持运行态；服务保留完整终态候选并启动单个重试 worker，初始间隔 1 秒、指数退避至 30 秒。重试只在 CAS 提交成功后投影和发布；关闭时若仍失败，持久行保持 running，由既有启动恢复处理。
4. session cleanup 先发进程 kill，再尝试取消 CAS；CAS 前保留 action entry，写入错误时由重试 worker 保持候选，提交后才从 board 删除。已经终态的条目只删除内存记录，不重复发送 `action:finished`。
5. `attach_session` 先提交现有事务化 `update_action_session`，成功后才改内存并发 `action:updated`；失败则保留原 owner。持久模式不重发 transient completion，未投递 outbox 会在事务中更新 owner 并由原恢复路径投递。无数据库模式没有 outbox，late attach 只补发带 owner 的 Agent completion。
6. 没有修改 `haven-memory` API 或 schema：现有完成/失败 CAS+outbox、取消 CAS、按 ID 读取和事务化 owner 更新已满足仲裁需要。transcript 投影后 ack、稳定 `action_result_id`、取消不进 durable outbox、重启恢复保持不变。

回滚只需回滚本次 ActionService 代码和该 ADR 的实现记录；没有数据库契约变化或重置要求。重试在应用关闭时停止，未提交候选不会伪装为成功，重启后由 `mark_interrupted_actions` 处理仍为 running 的 durable row。

## 验证与回滚

实现与测试位于 `crates/tools/src/action_service.rs` 和 `crates/tools/src/action_service_tests.rs`。回归覆盖完成对取消竞态、CAS 丢失、outbox 与取消写入错误、session cleanup 写入错误、重复完成、持久 late attach，以及 headless late attach 原有语义。

验收命令：`cargo fmt --all -- --check`、`cargo test --locked -p haven-tools`、`cargo clippy --workspace --locked -- -D warnings`。本切片无 schema 变更，无需重置数据库。
