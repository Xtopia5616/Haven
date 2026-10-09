# ADR 0821：保留重复暂停更新中的运行结束原因

## 状态

已接受（2026-10-09）

## 背景

用户主动打断会发布带有具体原因的 paused lifecycle event。被取消的运行退出时，dispatcher 还会发布一个不带原因的通用 paused 更新。两个异步事件的到达顺序不固定；若通用更新后到，SessionReducer 会把刚建立的 `SessionRunEndNotice` 清除，导致时间线偶尔不显示暂停原因。

## 决定

同一会话已有 paused `SessionRunEndNotice` 时，后续 paused 状态更新只更新会话摘要，不清除此提示。进入 pending/running 仍会清除旧提示；新的 `session/run-ended` 仍可替换提示内容。原因事件与通用状态事件的 wire contract 不变。

## 替代方案

- 要求每个通用暂停 producer 都传递原因：拒绝。运行退出通知没有可靠的业务原因来源，且仍不能消除分散 producer 的事件顺序竞态。
- 忽略所有重复 paused lifecycle event：拒绝。它们仍负责刷新状态和 waiting reason；只需保留已有运行结束提示。

## 影响与验证

仅调整 UI reducer 对同会话 paused 状态更新的清理规则。没有 IPC、数据库、配置或 transcript 变更，无需重置数据。验证采用 UI 静态检查；未运行测试。

## 回滚

恢复 `session/status-updated` 对同一会话无条件清理 `runEndNotice` 的规则即可；无持久化数据需要回滚。
