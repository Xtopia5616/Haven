# ADR 0465：删除 AppState 到 ApplicationRuntime 的隐式转发

## 状态

已采纳（2026-10-04）。

## 背景

`AppState` 实现 `Deref<Target = ApplicationRuntime>`，让 Tauri 命令和应用服务可以像 runtime 字段仍直接属于 `AppState` 一样访问它们。代码将此称为“command compatibility”。实际结构已经将长生命周期服务集中到 `ApplicationRuntime`，而 `AppState` 只额外持有 Tauri/UI 瞬态状态；隐式转发掩盖了这一所有权边界。

## 决定

1. 删除 `AppState` 的 `Deref` 实现。
2. 命令、通知、启动和配置运行时中的服务访问必须显式经过 `AppState.runtime`。
3. Tauri IPC、配置和数据库 schema 不变；这是 Rust 内部 API 收紧。

## 替代方案

- 保留 `Deref` 并依靠注释说明真实所有者：拒绝。所有者在调用点仍不可见，新增字段访问会继续沿用隐式兼容入口。
- 将所有 Tauri 瞬态字段并入 `ApplicationRuntime`：拒绝。这会把 UI recording、hotkey capture 和确认状态混入通用应用服务生命周期。

## 影响与验证

调用点现在明确标出 runtime 所有权，`AppState` 仅作为 Tauri 注册状态及其专有瞬态状态容器。Tauri command 和前端 wire contract 不变。

验证：`cargo check --locked -p haven-app-binary`、workspace 格式与严格 Clippy；本次未运行测试或 UI 检查。

## 回滚

若需要恢复源码 API，可恢复 `Deref<Target = ApplicationRuntime>` 并移除调用点的显式 `runtime` 字段。该变化不触及持久数据或用户配置。
