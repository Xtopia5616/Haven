# ADR 0675：统一 Settings 运行时应用类型名

## 背景

App 配置运行时协调器名为 `SettingsRuntimeApplyCoordinator`，但它使用的上下文、计时器、阶段、计划、观测、失败类别与执行结果类型分别缩写为 `SettingsApplyContext`、`SettingsApplyTiming`、`SettingsApplyPhase`、`SettingsApplyPlan`、`SettingsApplyObservation`、`SettingsApplyFailureKind` 和 `SettingsApplyOutcome`。同一职责族词根不一致，而且 `SettingsApplyPlan` 容易与 `RuntimeConfigApplyPlan` 混淆。

这两个 plan 承担不同阶段：`RuntimeConfigApplyPlan` 将变更域映射成 live/restart targets；Settings plan 再根据变更快照及 hotkey 变化，派生实际执行顺序。

## 决定

- Settings 上下文、计时器、阶段、计划、观测、失败类别与结果统一改名为 `SettingsRuntimeApplyContext`、`SettingsRuntimeApplyTiming`、`SettingsRuntimeApplyPhase`、`SettingsRuntimeApplyPlan`、`SettingsRuntimeApplyObservation`、`SettingsRuntimeApplyFailureKind`、`SettingsRuntimeApplyOutcome`。
- 阶段顺序常量改名为 `SETTINGS_RUNTIME_APPLY_PHASE_ORDER`。
- 保留 `RuntimeConfigApplyPlan` 作为变更域到 runtime targets 的映射计划，不与有序 Settings 执行阶段计划合并。
- 不保留旧 Rust 名称 alias。此改动只改 App 内部符号名，不改设置 IPC、配置映射、执行阶段、错误处理或运行行为。

## 考虑过的方案

- 保留 `SettingsApply*`：协调器与其紧邻类型族仍使用不同词根，计划角色也不够清楚。
- 合并两种计划：映射结果与阶段顺序处于不同抽象层，合并会令单一结构同时承担配置分类与执行编排。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --locked -p haven-app-binary`
- ADR 索引检查与 `git diff --check`
- 未运行测试；本轮只执行格式与编译门禁。

## 回滚与重置

将 `SettingsRuntimeApply*` 内部符号恢复为旧名即可回滚。没有配置、持久化或 wire shape 变化，无需重置。
