# ADR 0830：回报输入注入证据并验证窗口关闭

## 状态

Accepted — 2026-10-09

## 背景

输入类工具除 `input.key` 外只回报请求动作，不回报 Windows 是否完整接收 SendInput 事件。UI Automation 的 invoke/value/toggle/select 错误也可能在请求已派发后发生。窗口关闭通过异步 `WM_CLOSE` 投递完成，但工具原先把“消息已投递”直接表述为“窗口已关闭”。

## 决定

- 所有 SendInput 输入操作在成功结果中返回 `accepted_events`。这证明 Windows 接收了全部事件，不证明应用处理了这些事件。
- UI Automation 的动作调用发生后若 COM 返回错误，将结果标为 `unknown_outcome`，提示控件可能已经改变；查找控件、取得 pattern 等派发前错误仍保留普通失败分类。
- `window.close` 投递 `WM_CLOSE` 后最多等待 2 秒，周期检查原 HWND 是否仍属于原进程。只在句柄消失或不再对应原进程时返回 `verified_closed: true`；超时或投递边界失败返回未知结果。
- 窗口关闭验证以 HWND/进程身份为边界，不推断应用数据是否已保存。

## 替代方案

- 把 `SendInput` 全部事件接收等同于应用层成功：拒绝。OS 注入不提供应用业务状态。
- 把 `WM_CLOSE` 投递成功等同于窗口已关闭：拒绝。窗口可能弹出未保存确认或不处理请求。
- 在 UI Automation 错误后自动重放动作：拒绝。toggle/invoke 等重复可能造成额外副作用。

## 影响与验证

输入结果动态 JSON 新增 `accepted_events`，窗口关闭结果新增 `verified_closed`；UI renderer 和 shape validator 已同步。窗口关闭在 blocking worker 上最多等待 2 秒，不阻塞异步执行器线程。没有 IPC DTO、配置或持久数据变化。适用门禁：Rust workspace 编译、严格 Clippy、Rust 格式检查、UI 类型检查与 diff 检查。测试未运行；真实 SendInput/WM_CLOSE 行为仍需 Windows 桌面会话验收。

## 回滚

恢复原结果字段和立即返回 WM_CLOSE 的行为即可；无需配置或数据库重置。
