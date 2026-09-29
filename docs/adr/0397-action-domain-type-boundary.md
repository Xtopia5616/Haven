# ADR 0397：定时任务类型归入 Action 域

- 状态：已采纳（2026-09-29）
- 范围：ActionService 使用的 scheduled domain types 与 `haven-actions` crate 边界评估
- 关联：ADR 0305、0343、0352、0392、0393、0396

## 背景

`ScheduleMode` 和 `ScheduledActionSpec` 定义在 `builtin::scheduled_action`，但
`ActionService`、Agent 与 app composition root 都直接使用它们。`ScheduledActionFired`
则定义在 completion transport 中。Action owner 因而反向依赖 builtin tool 实现，scheduled
domain types 也分散在工具与传输模块。

`ActionService` 还直接使用 Tools 内部的进程读取/终止、shell 命令构造、输出日志与 shell
输出策略 helper。仅迁移 ActionService 会留下对 Tools 的反向依赖；让 `haven-tools` 再依赖
`haven-actions` 会形成循环。

## 决定

1. 新增 `haven-tools::action_types` 作为现阶段的 Action 域类型模块，集中定义
   `ScheduleMode`、`ScheduledActionSpec` 与 `ScheduledActionFired`。Tool 专属的
   `ScheduleOperation`、`ScheduledActionParams` 与 `ScheduledActionTool` 继续归 builtin。
   Agent、ActionService 和 app composition root 使用 Action 域类型，不再从 scheduled
   builtin 取得领域模型。
2. 保留一个统一的 `ActionService`，继续负责 background/scheduled admission、状态、触发、
   进程生命周期及既有持久化边界；本次不增加第二个 service 或 executor，也不改变类型的
   Serde 形状、数据库字段、IPC 或 completion 语义。
3. 本次不新增 `haven-actions` crate。单独成 crate 前，先确定共享进程与 shell helper 的正确
   所有者或注入边界，使 ActionService 不依赖 `haven-tools`，同时避免把有行为的 shell 执行
   逻辑放进 `haven-common`。满足该依赖边界后，再依据 Agent、Tools 与 App 的实际调用图评估
   crate 拆分；不以拆 crate 本身作为目标。

## 替代方案

- 让 ActionService 继续接收 `builtin::scheduled_action` 的类型：拒绝。它会继续让领域 owner
  依赖具体工具实现。
- 为 background 与 scheduled 各建一个 service：拒绝。两类 action 仍共享状态、持久化、
  完成 transport 与 UI 投影契约；执行差异继续由 kind-specific owner 处理，符合 ADR 0393。
- 现在直接新增 `haven-actions` 并依赖 `haven-tools`：拒绝。这会形成循环依赖；复制或上移
  shell/process helper 只为满足 crate 图也会移动行为边界，而本次没有为这些 helper 采纳新 owner。

## 影响与验证

scheduled domain types 现在只有 `action_types` 一个定义位置；builtin 和 app 调用点改用该
域 API。保留 `haven_tools::ScheduleMode` 根 re-export，并在根导出 `ScheduledActionSpec` 与
`ScheduledActionFired`。不改 schema、用户数据、事件、状态机或工具输入/输出语义，不需要数据重置。

验收覆盖现有 scheduled tool、ActionService、Agent consumer 和 app 恢复测试；静态检查使用
`cargo fmt --all -- --check`、`cargo check --workspace --locked`、
`cargo clippy --workspace --locked -- -D warnings` 与 `cargo test --workspace --locked`。

## 回滚

将三个类型恢复至原模块、还原其调用路径并删除 `action_types` 即可；没有数据库或配置迁移。
