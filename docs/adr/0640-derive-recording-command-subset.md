# ADR 0640：从生成命令契约派生录音控制器命令子集

## 状态

已采纳并实施。

## 背景

`RecordingOverlayControllerDependencies.invoke` 只允许录音 overlay 发起 `start_recording`、`stop_recording` 和 `cancel_recording`，但 controller 与测试各自手写了一份相同 literal union。Tauri 命令名已由 `generatedCommands.ts::TauriCommandName` 从 Rust handler 生成；局部 union 没有成为另一命令 owner 的理由，也会在重命名 IPC command 时漂移。

## 决定

1. controller 使用 `RecordingCommandName = Extract<TauriCommandName, 'start_recording' | 'stop_recording' | 'cancel_recording'>` 表示其有意收窄的命令能力。
2. 测试 mock 使用 `RecordingOverlayControllerDependencies['invoke']` 作为函数类型，不重复声明命令集合。
3. 命令的 canonical 名称和值继续归 generated IPC contract 所有；controller 只拥有调用范围。

## 替代方案

- 直接接受完整 `TauriCommandName`：拒绝。这样会意外扩大录音 controller 能调用的命令集合。
- 继续在生产与测试代码分别维护 literal union：拒绝。两个集合表示相同约束，存在漂移风险。

## 影响与验证

此变更仅收敛 UI 内部的静态类型来源；Tauri 命令、payload、调用时序与运行时行为不变。无需保留旧内部 alias。验证：`corepack pnpm run check`、`corepack pnpm run test:run`、ADR 索引与 staged diff 检查。

## 回滚

如需回滚，恢复 controller 和测试的局部 literal union，并同步撤回命名规范、路线图和索引记录；无 IPC 或持久化影响。
