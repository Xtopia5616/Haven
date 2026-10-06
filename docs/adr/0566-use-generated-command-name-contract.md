# ADR 0566：以生成契约统领 IPC command 集合

## 状态

已采纳并实施。

## 背景

`generatedCommands.ts` 从 Rust handler 注册生成 `TauriCommandName` 与每个 command 的 request/response shape。`contracts/commands.ts` 是 renderer 审阅的 boundary/security metadata directory，却又从自己的对象键推导同名 `TauriCommandName`，并以 `Record<string, CommandContract>` 检查值；若 generated handler 新增但目录未登记，类型层不保证能发现。

## 决定

1. generated `TauriCommandName` 是 command 集合和调用类型的唯一 owner。
2. 安全目录以 `Record<TauriCommandName, CommandContract>` 约束所有键和值，要求每个 generated command 都有 reviewed 元数据。
3. 移除 `commands.ts` 的重复 `keyof typeof TAURI_COMMAND_CONTRACTS` 类型；command 顺序、metadata 内容和 Tauri 调用行为不变。

## 替代方案

- 保留两个相互推导的 command-name unions：拒绝。它们来自不同列表，会因遗漏而漂移。
- 仅依赖运行时 `check-ipc-contracts.ps1` 对数量/元数据做 parity 验证：拒绝。编译期完整 `Record` 可直接约束 UI 目录自身。
- 让 generated contract 引用 renderer 安全目录：拒绝。生成契约由 Rust handler 权威定义，不能反向依赖 UI metadata。

## 影响与验证

- 只调整 UI 命令 metadata registry 的类型约束与命名来源；generated IPC 与 wire shape 不变。
- 验证：`scripts/check-ipc-contracts.ps1`、UI `check`、`test:run`、`build`、ADR 索引与差异空白检查通过。

## 回滚

恢复 `commands.ts` 基于 `keyof typeof TAURI_COMMAND_CONTRACTS` 的本地 `TauriCommandName`，并将 registry 约束恢复为 `Record<string, CommandContract>`。
