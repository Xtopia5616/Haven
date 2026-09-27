# ADR 0378：删除重复的定时任务完成 helper

- 状态：已采纳（2026-09-27）
- 范围：Memory `scheduled_actions` repository
- 关联：[ADR 0373](0373-tools-action-admin-writers-audit.md)

## 背景

`Database::complete_scheduled_action` 是 `finish_scheduled_action` 之前留下的便利入口，复制了一份 SQL 并把终态固定为 completed。当前 runtime 已通过 `finish_scheduled_action` 显式提交终态、错误原因与完成时间；旧 helper 没有生产调用方，仅被 repository 测试使用。

## 决定

删除 `complete_scheduled_action`，包括重复 SQL。需要完成 scheduled action 的调用方统一使用 `finish_scheduled_action`，显式传递终态和可选错误原因。

## 影响、重置与验证

- 不改 schema、数据库行、Action lifecycle、配置或 IPC，不需要用户数据重置。
- repository 测试改用唯一终态写入入口；运行 Memory crate 测试。

## 回滚

恢复 helper 及其重复 SQL 和测试调用；无需数据迁移。
