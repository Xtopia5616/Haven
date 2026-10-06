# ADR 0540：删除未使用的 Shell 快捷键配置副本

## 状态

已采纳并实施；`haven-app-binary` crate 门禁通过。

## 背景

`haven_common::config::HotkeyConfig` 是应用设置中的实际快捷键配置，持有模式、主键位与可选静音键位，并被配置服务、设置命令和输入注册流程读取。App 私有 `desktop::HotkeyConfig` 则保留 recording/toggle 两个字符串，只在 `ShellState::default` 写入；生产代码从未读取这些字段，ShellState 也没有被序列化为 IPC 或其他外部契约。它是旧形状的未使用副本，同名让配置审查和符号搜索难以分辨。

## 决定

1. 删除 `desktop::HotkeyConfig`、`ShellState.hotkey` 初始化以及只检查旧默认字符串的测试。
2. 保留 Common `HotkeyConfig` 作为唯一快捷键配置；不把用户设置复制到 ShellState。
3. 保持 Tauri commands/events、持久化配置、快捷键注册/重绑流程与运行行为不变。

## 替代方案

- 将 App 类型改名为 `ShellHotkeyBindings`：拒绝。代码没有读取者或独立生命周期，改名会继续保留无效数据模型。
- 将 Common 配置复制进 ShellState：拒绝。快捷键配置的权威 owner 已是 ConfigService；复制会形成可能过期的运行态副本。
- 合并为跨 crate DTO：拒绝。App Shell 状态不是配置 API，合并会扩大公共边界且没有消费者收益。

## 影响与验证

- 删除仅存在于 App 私有 `desktop` 模块的字段与类型。Common 设置格式、数据库、IPC/event contract 和快捷键副作用均不变；无需数据重置或迁移。
- 验证通过：`cargo fmt --all -- --check`、`cargo check --locked -p haven-app-binary`、`cargo clippy --locked -p haven-app-binary -- -D warnings`、`cargo test --locked -p haven-app-binary`（228 passed）、`scripts/check-adr-index.ps1` 与 `git diff --check`。

## 回滚

恢复 `desktop::HotkeyConfig` 及 `ShellState.hotkey` 默认初始化和默认值测试即可；无需恢复配置、数据或 wire contract。
