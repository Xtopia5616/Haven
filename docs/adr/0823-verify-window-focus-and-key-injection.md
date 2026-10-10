# ADR 0823：验证窗口聚焦与按键注入结果

## 状态

Accepted — 2026-10-09

## 背景

`window.focus` 调用 `SetForegroundWindow` 后忽略其结果，也不读取实际前台窗口；因此它可能在目标未获得焦点时仍返回成功。`input.key` 只报告请求的快捷键，但没有说明 Windows 接收了多少输入事件或事件发送时哪个窗口处于前台。调用者会把“API 已调用”误读为“目标窗口已收到操作”。

## 决定

- `window.focus` 在请求聚焦后读取前台窗口；只有实际前台 HWND 与目标一致才返回成功，并在 `foreground_window` 中返回观察到的窗口身份，同时设置 `verified_foreground: true`。不匹配时返回 `unknown_outcome`，错误信息记录请求与实际前台的窗口 ID。
- Windows `SendInput` 返回已接受的事件数。`input.key` 在注入前后读取前台窗口；仅当所有按键事件被接受且前台窗口身份保持一致时返回成功，并附 `accepted_events`、前台身份及 `verified_foreground: true`。注入部分失败、注入后无法读取前台或窗口身份改变时返回 `unknown_outcome`。
- 结果证明的是前台目标身份与 OS 输入注入状态，不证明应用已执行快捷键对应的业务动作。调用方仍须使用窗口观察/状态读取来验证应用层效果。
- UI 专用 renderer 展示这些证据；shape guard 校验消费字段，异常 payload 回退 JSON。

## 替代方案

- 继续把 `SetForegroundWindow`/`SendInput` API 调用成功当作操作成功：拒绝。它不能说明哪个窗口处于前台，也不能说明输入是否完整注入。
- 在失败后自动重试按键：拒绝。第一次注入可能已经部分生效，重复快捷键会造成额外副作用。
- 宣称应用已完成快捷键对应动作：拒绝。没有通用方式推断任意应用的语义后置条件。

## 影响与验证

IPC 与持久数据不变；Window/Input `ToolResult.output` 继续使用动态 JSON，增加验证字段。聚焦未验证、输入不完整或前台漂移的副作用失败使用 `unknown_outcome`，不允许呈现为成功。

适用门禁：`cargo fmt --all -- --check`、`cargo test --locked -p haven-tools`、`cargo clippy --locked -p haven-tools -- -D warnings`、`cd ui; corepack pnpm run check`、`cd ui; corepack pnpm run test:run`。实际 Windows 前台切换与 `SendInput` 接收行为仍须在 Windows 桌面会话验收。

## 回滚

恢复原有 focus/key 输出与错误分类会重新引入把 API 调用误当成操作成功的风险。仅涉及进程内行为与动态结果，不需要数据库或配置重置。
