# ADR 0519：验证 Session grant 与 resolve event 分离写入的失败语义

## 状态

已完成（2026-10-06）。

## 背景

Session-owned grant-aware confirmation 有两个顺序执行的 durable write：

1. `SessionSupervisor::grant_session_permission` 先写 `session_authorization_grants`，成功后更新当前 `AuthorizationEngine`。
2. 随后 `SessionActor` 追加 `interaction_resolved`；只有追加成功才改变 actor 内存中的 pending 状态并允许推进。

ADR 0424 已明确两次写入不具备原子性。若第二步失败，当前契约是 grant 已生效、resolve 返回可重试错误、请求继续归原 Session owner 且保持 pending；renderer 保留待处理 UI。在本 ADR 实施前，测试分别覆盖了 grant 持久化先于 scheduled tool wake，以及 Session confirmation batch 的 `interaction_requested` append 失败回滚，但没有覆盖 Session grant 成功后 `interaction_resolved` append 失败的组合边界。

路线图步骤 0 复核确认这是授权与生命周期状态之间的确定性持久化失败点，满足 §5.6 步骤 2 的风险特定故障注入准入条件。本缺口没有实际回归证据，不满足启动 grant/event 事务重构或重划 owner 的条件。

## 决定

仅添加一个 Session-owned confirmation 故障注入测试，固定以下现有契约：

- `interaction_resolved` insert 被 SQLite trigger 拒绝后，grant 行仍持久化，当前授权引擎也能看到该 grant。
- resolve 调用返回可重试错误；同一 Session actor 中原请求仍 pending，session 仍 Paused，durable event log 没有 `interaction_resolved`。
- 移除注入故障后，使用同一 request ID 重试可以成功；pending 请求消失，session 恢复，durable log 恰有一条对应的 `interaction_resolved`，grant 没有重复行。

测试归入 `tool_runner.rs` 的授权/确认 owner 测试，因为它要真实组合 grant-aware resolver、SessionSupervisor、SessionActor 与 SQLite。若无法装配真实 continuation，本测试只对持久化与 owner 状态作断言，不把未启动的测试工具误写成执行保证。

## 保持不变与非目标

- 不改变 grant-before-resolve 顺序，不增加补偿删除，不声称两次写入原子。
- 不更改 SessionStore/SessionActor 的 durable decision owner、event payload、schema、IPC、renderer 重试行为或其他确认 owner。
- 不将 ScheduledAction / AppCommand 或 actor 在两次写入间停止的时序并入本切片。后者的 stale/retry 语义依 actor registry 与 mailbox 状态而变，尚未定义。
- 不据此重构 `session_events.rs`、合并 grant 和 event transaction，或拆分 crate。

## 验收与停止条件

- 故障注入准确落在 `interaction_resolved` append，证明失败发生在 durable grant 写入之后。
- 失败与恢复断言覆盖上文列出的 grant、live authorization、pending owner、Paused 状态、event 数和同请求重试。
- 运行 `cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、crate dependency inventory、ADR index 与 `git diff --check`。
- 若观测行为与上述既有契约不一致，停止此测试切片并先修订授权/交互契约；不以测试期望掩盖运行时语义。

## 回滚

移除该定向测试和 ADR 0519，并将 roadmap 的 Active 恢复为无；本切片不修改生产运行时代码、schema 或用户数据。

## 实施结果

- 在 `crates/agent/src/session/tool_runner.rs` 增加 Session-owned 故障注入测试，以 SQLite trigger 拒绝 `interaction_resolved` insert。
- 测试确认 resolve 返回错误后，grant 已持久且当前授权引擎返回 `AutoApproved`；原请求仍由 actor 持有、session 保持 Paused，event log 中无 decision event。
- 移除 trigger 后，同一 request ID 的重试完成 resolve，session 恢复、pending 消失，只存在一条 resolved event 和一条 grant。
- 通过：定向测试；`cargo fmt --all -- --check`；`cargo test --workspace --locked`；`cargo check --workspace --locked`；`cargo clippy --workspace --locked -- -D warnings`；crate dependency inventory；ADR index；`git diff --check`。
- 未修改生产运行时代码、schema、IPC 或用户数据；actor-stop stale/retry 语义仍单独 Deferred。
