# ADR 0638：合并 SessionRun 结束提示状态

## 状态

已采纳并实施。

## 背景

`SessionReducerState` 同时保存 `error: SessionError` 和 `termination: SessionTermination`。同一活动错误会把 `sessionId` 与 `reason` 写入两者，错误与结束提示的 reducer 分支还要分别清理、恢复和保留它们。`paused` 也包含在 `SessionTerminationStatus` 中，但它表示本次运行暂停，不是对话实体终止。

历史错误原因缓存 `sessionErrorReasons` 与当前 UI 提示承担不同生命周期：前者按 session 保留，供历史重开时恢复；后者只描述当前活动 session 的本次运行结束结果。两者不合并。

## 决定

1. 将当前结束提示统一为 `SessionRunEndNotice { sessionId, status, reason }`，状态类型 `SessionRunEndStatus` 从 generated `SessionStatus` 提取 paused/completed/error。
2. 删除重复的 `SessionError` state；页面根据 `runEndNotice.status === 'error'` 推导错误继续操作状态，不再单独传活动错误布尔值或错误原因。
3. 合并 `session/error-shown` 与 `session/termination-shown` 为 `session/run-ended`。该 action 在一个 reducer transition 中更新 session summary 的 status/title/waiting reason，并为活动 session 写入唯一的 run-end notice。继续生成成功后的显式清除 action 命名为 `session/run-end-notice-cleared`。
4. 将状态字段改为 `runEndNotice`，展示组件和 timeline props 改为 `SessionRunEndBanner`、`runEndStatus` 与 `runEndReason`。session 继续表示对话，结束提示说明一次 SessionRun 的状态。
5. 保留 `sessionErrorReasons` 及其 remember/forget/read API，因为它是跨当前提示生命周期的按 session 历史缓存。

## 替代方案

- 保留 `error` 并只改名 `termination`：拒绝。错误和结束状态仍需同步维护，两份状态里存有同一错误 identity/reason。
- 合并历史错误原因缓存：拒绝。它会跨当前 UI 提示生命周期存在，职责和清理时机不同。
- 继续使用“session termination”描述 paused 状态：拒绝。paused 结束的是当前 session run，对话可以继续。

## 影响与验证

- 变更只涉及 UI reducer、生命周期 action、chat timeline 展示和内部组件命名。
- Rust、Tauri IPC、事件 payload、持久数据与配置均不变；不需要向下兼容旧的 UI 内部 reducer action/state。
- 验证：`corepack pnpm run check`、`corepack pnpm run test:run`、ADR 索引与 staged diff 检查。

## 回滚

恢复 `SessionError` 与 `SessionTermination` 双状态、旧 reducer actions 和组件名，并同步恢复旧测试与调用点；不涉及持久数据或 wire rollback。
