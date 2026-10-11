# ADR 0984：统一校验 Tauri event envelope id

## 状态

已采纳并实施。

## 背景

Tauri renderer event envelope 的 `id` 是本地事件序号，不是领域实体 ID。Agent mapper 要求安全的非负整数，App、Session 和 ToolRun mapper 却只拒绝非有限值；负数、小数和超过 JavaScript 安全整数范围的序号会被不一致地接受。

## 决定

在 `tauriEvent.ts` 定义唯一 `isValidTauriEventId` guard，要求非负、安全整数。Agent、App、Session、ToolRun 的 event mapper 一律通过该 guard 校验 envelope；payload 内数值仍按各自 Rust wire 类型或领域范围校验。

## 影响与验证

- 只拒绝无效的本地 event envelope id，不改变 Rust payload 或领域 identity。
- UI mapper 单测覆盖负数、小数、非有限值和超安全整数。

## 回滚

恢复各 mapper 原有的 event id 检查并移除共享 guard 与对应测试。IPC payload、持久化和事件注册不变，无需数据迁移。
