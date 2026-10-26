# ADR 1425：增加 PermissionRequest 与 ToolRun 生命周期 Hook

## 状态

已实现并通过门禁。

## 背景

现有 `pre_tool_use` 在用户确认之后、工具执行之前运行，无法在确认弹窗出现前拒绝某项需要确认的操作。后台 ToolRun 已有持久化的创建和终态，但 Hook 只覆盖 Agent run 和前台 ToolCall，无法可靠接入脱离当前 turn 的后台任务生命周期。

## 决定

- 增加 veto 型 `permission_request`。仅当 Haven 的授权决议为 `RequiresConfirmation` 时，在创建常规用户确认请求前运行。它接收有界的工具名、工具输入、session/run ID 和 `step_number`。`allow` 或空 stdout 只继续到 Haven 自己的确认弹窗；只有用户的常规确认能批准操作。显式 `deny`、运行失败、超时、取消、安全策略拒绝、无效响应或超大 payload 均阻止操作；自动批准、策略阻止和恢复时已有的确认决定不触发该事件。
- 增加通知型 `toolrun_start` 与 `toolrun_end`。`toolrun_start` 在 ToolRun running 行持久化且 session owner 已绑定后派发；`toolrun_end` 在 terminal 状态持久化且 owner 已知后派发。若任务在 owner 绑定前就结束，绑定事务提交后依次派发 start 和 end Hook。Hook 因此始终使用实际 owner session 的工作区与项目 Hook 信任范围。
- 保留 `ToolRunService` 现有的迟延 session 绑定边界：Agent 只有确认 session 仍在运行集合后才调用 `attach_session`。绑定事件携带提交后的状态快照，使 app 能恢复早于绑定发生的 terminal 生命周期通知，不提前改变 rollback/cancel 仲裁。
- 两个事件均为异步 best-effort 通知，进程可能在持久化与 Hook 派发之间退出，因此不提供 outbox、重放或 exactly-once 保证。
- ToolRun Hook 使用一个 app-owned 顺序 worker 和 32 个 event-batch 的有界队列；队列满时丢弃新 batch 并记录静态诊断，应用退出时取消当前 Hook。每个 batch 最多含 start/end 两个 payload。
- ToolRun payload 仅提供 `tool_run_id`、`session_id`、`source_step_id`、工具名和状态；`toolrun_end` 额外提供是否成功。它不包含原始输入、输出或错误文本。matcher 按工具名匹配。
- 两类 Hook 都加入用户级与项目级配置、设置编辑器、诊断和生成的 IPC 契约。配置缺省为空，Hook 命令沿用现有安全授权、执行边界、进程树清理、事件预算和 payload 上限。

## 替代方案

- 用 `pre_tool_use` 代替 `permission_request`：拒绝；它发生在用户已确认之后，无法阻止确认弹窗。
- 让 `permission_request` 的 allow 跳过用户确认：拒绝；Hook 只能否决，不能授予 Haven 权限或代表用户批准。
- 在 ToolRun 创建时提前绑定 session：拒绝；这会绕过执行器确认 session 仍然有效的 rollback/cancel 仲裁边界。
- 在 owner 绑定前直接运行项目 Hook：拒绝；此时还不能安全地选择该 ToolRun 的执行工作区与受信项目 Hook。
- 将 ToolRun 输出和错误传给 Hook：拒绝；后台结果可能包含敏感工作区内容，审计通知只需稳定身份与状态。
- 用持久 outbox 保证 Hook 投递：拒绝；Hook 是可选外部副作用，本次不增加其存储、重试和幂等协议。

## 影响与迁移

不修改数据库 schema、session event 或既有 ToolRun 完成契约，无需数据库重置。新增的可选配置键是 `permission_request`、`toolrun_start` 和 `toolrun_end`；旧版严格配置解析器不接受这些字段。回滚时应从 `config.toml` 与受信项目 `.haven/hooks.toml` 移除这三个键。

## 验证

已通过 `cargo fmt --all -- --check`、crate 依赖与 ADR 索引检查、IPC 契约和事件检查、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`，以及 UI `check`、全量测试和生产构建。Windows MXC workspace-write Hook 测试也通过。
