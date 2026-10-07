# ADR 0684：删除未使用的 Config Admin 构造兼容入口

## 状态

已采纳并实施。

## 背景

`ConfigAdminContext`、`ConfigAdminTool`、`ConfigAdminOperation::new` 和 `new_config_admin_tool` 保留了一条只构造 Config Admin operation 的独立路径，注释称用于单独构造该操作的 callers。全仓生产代码没有调用方；真实 catalog composition 已经由 `AdminSurfaces` 创建共享 `AdminServices` 并持有 `ConfigAdminOperation`。这些旧类型还从 `haven_tools` crate root re-export。

## 决定

- 删除窄 `ConfigAdminContext` 及到宽 `AdminContext` 的转换。
- 删除独立 `ConfigAdminOperation::new`、公开 `ConfigAdminTool` alias 与 `new_config_admin_tool` builder。
- `ConfigAdminOperation` 仅作为 `AdminSurfaces` 的 crate 内组成，并从 crate-root/builtin exports 移除。
- 配置操作测试用 test-only helper 构造相同的 `AdminServices`、`ConfigAdminOperation` 和 `TypedToolAdapter`，不恢复生产备用构造路径。

## 替代方案

- 保留该 public helper 以供潜在外部调用者使用：拒绝，当前 workspace 没有生产消费者，且测试版本不需要向下兼容 API。
- 用 `AdminContext` 取代 `ConfigAdminContext` 但继续导出独立 builder：拒绝，仍会保留绕过组合 owner 的第二条装配路径。

## 影响与验证

- 删除未使用的 Tools Rust API 与 crate-root exports。当前五个 Admin tool names、typed operation、共享 services、日志级别应用和配置读写行为不变。
- 不涉及配置或持久化，无需重置。
- 已执行 Rust workspace 编译、严格 Clippy 与格式检查；测试未运行。

## 回滚

恢复窄 context、adapter alias、独立构造函数和导出即可；无需数据重置。
