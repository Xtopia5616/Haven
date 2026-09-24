# ADR 0275：ActionService 以 typed agent projection 作为工具边界输入

- 状态：Accepted
- 日期：2026-09-24
- 范围：ActionService 的 agent-facing status/list 查询与 `actions` builtin tool
- 关联：[ADR 0215](0215-action-board-typed-projection.md)、[ADR 0248](0248-background-action-terminal-commit-order.md)

## 背景

ActionService 已有面向 UI 的 `ActionView`，但 agent-facing 的后台 action、定时
action、运行中输出、终态结果和未找到结果具有不同 JSON 形状。原实现让
`list_for_session`、`status_for_session` 和 `ActionsTool` 直接用
`serde_json::Value` 读写 `status`、`kind` 和 `preview`，导致领域状态与模型工具
协议混在同一层；同时 `ActionService` 的完成通知仍需要保持既有 JSON 形状。

## 决策

ActionService 增加内部 `ActionStatusView`、`ActionStateView` 与 `ActionListView`。
`status_view`、`status_for_session_view` 和 `list_for_session_views` 先产生 typed
projection；`ActionsTool` 在应用过滤、判断全量 running 和组装工具结果时只使用
typed 状态，最后一次性转换为既有 JSON。旧的 `status`、`status_for_session` 和
`list_for_session` 方法保留为兼容 serializer，完成 outbox、生命周期事件和 UI
`ActionView` 不复用这组 agent DTO。

动态 `tool_args` 仍保持 `serde_json::Value`，因为它是外部工具参数扩展边界；数据库
schema、IPC shape、字段名、排序、运行中等待提示和完成通知均不变。

## 影响与验证

- `ActionsTool` 不再通过 JSON 字段索引进行状态过滤或 running 判断；
- UI projection、agent tool projection、completion notification 三者职责明确；
- typed projection 与 legacy JSON serializer 有回归测试；
- 通过 `cargo test --locked -p haven-tools action_service`、`cargo clippy --locked -p haven-tools -- -D warnings`，并需通过 workspace 全量门禁。

## 回滚

删除 typed view 方法并恢复 `ActionsTool` 对兼容 JSON wrapper 的调用即可，无数据迁移，
不改变 action 状态或 outbox。
