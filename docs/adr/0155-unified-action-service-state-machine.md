# ADR 0155：统一 ActionService 状态机

## 状态

已接受（2026-09-14）

## 背景

后台 shell 与定时任务虽然共用 `actions` 表，却分别维护内存 registry、数据库 setter、事件 sink
以及 completion/fired channel。action watch 因此必须通过 `set_actions` 把两个 registry 重新接线，
取消、恢复和 action board 也存在按 kind 分叉的运行时路径。

## 决策

`haven-tools::ActionService` 成为后台与定时任务的唯一 owner：

- 一个 `act-*` keyed action map 保存 process 与 timer/dependency action；
- `Waiting → Running → Completed | Failed | Cancelled` 是统一生命周期，timer/dependency
  的等待阶段明确为 `Waiting`；
- shell worker、timer worker 和 action-watch worker 只持有短期执行句柄，完成通知通过一个
  `ActionCompletion` broadcast bus 发送；
- 数据库仍是重启后的持久历史与恢复来源，`kind` 只描述 `ActionSpec`/存储 payload，不再决定
  谁拥有内存状态；
- 删除 `BackgroundActions`、`ScheduledActionCenter` 两个运行时类型，禁止新增 registry 或
  `set_actions` 接线。

## 影响与安全

所有 model、agent、Tauri action board 和 session cleanup 路径都通过 `ActionService` 查询、
取消和恢复。session-scoped 查询仍在 service 内校验 owner；动态 tool args、continuation
prompt 和 output-log path 仍只在内部 payload 使用。未知 action dependency 会进入等待态并在
producer 不存在时以 `not_found` 完成，避免 detached worker 永久悬挂。

## 验证与回滚

验证统一 shell→watch→scheduled fire、timer persistence/restore、owner-scoped cancel、
跨 kind board 和现有 background/scheduled 行为测试。回滚只需恢复本 ADR 对应提交；无需数据库
迁移，`actions` 表的既有 `kind`/payload 列保持兼容。
