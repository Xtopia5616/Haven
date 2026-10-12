# ADR 1108：Settings 在 renderer 边界映射 LLM 配置事件

## 状态

已接受（2026-10-13）

## 背景

SettingsView 使用通用 `registerListeners` 直接注册 `llm:config_changed`。这一入口接受任意字符串并绕过 `appEventListeners` 对生成 channel 和 payload 的校验；其他 app-shell 事件均先经领域 mapper。

## 决定

- SettingsView 的 LLM 配置事件通过 `appEventListeners` 注册，再交给 `registerListeners` 统一管理 mount/dispose 生命周期。
- IPC contract checker 增加回归断言，确保该监听经过 app event mapper 且不回退到 raw channel 注册。

## 影响

事件 payload 仍为 `null`，行为不变；renderer 监听入口现在使用生成的 channel union 和集中 mapper。没有 wire、持久化或配置变化。

## 验证

- `corepack pnpm run test:run` 与 `corepack pnpm run check` 在 UI 切片完成后运行。
- `pwsh -NoProfile -File scripts/check-ipc-contracts.ps1`。

## 回滚

不得让生产 renderer 监听器绕过对应领域事件 mapper。若新增通用事件 owner，必须保持由生成的 channel 类型约束并集中校验 payload。
