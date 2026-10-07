# ADR 0694：生成 VAD status event 契约

## 状态

已采纳并实施。

## 背景

`events.rs::VadStatusEvent` 是 `recording:vad_status` 的 Rust wire DTO，但该事件不由 Tauri command 返回，IPC generator 之前只显式导出了 `SessionLifecycleEvent`。UI `recording.ts::VadStatusPayload` 因而重复声明 `{ signal: string, state: string }`。两个字段没有 snake_case/camelCase 差异，结构可直接共用。

架构契约已明确 VAD signal/state 保持开放字符串：mapper 忽略附加字段、对畸形值使用安全默认值，并透传未知字符串，以便未来增加状态值而不让旧 renderer 丢弃事件。

## 决定

- IPC generator 显式导出 `VadStatusEvent`，使 event-only wire DTO 进入生成契约。
- UI `VadStatusPayload` 直接引用生成的 `VadStatusEvent`，删除手写重复字段结构。
- 保留字符串字段和当前 mapper 行为；不把 `VadSignal` / `VadState` 的内部枚举直接暴露为 IPC 类型。

## 替代方案

- 保留 UI 手写接口：拒绝。它与 Rust wire DTO 完全同形且没有独立 UI 投影职责，类型漂移只能在运行时发现。
- 将 signal/state 改为封闭 enum：拒绝。架构现有契约有意允许未知字符串透传；内部 VAD 状态也不等于 App event 的 wire 词汇。

## 影响与验证

- 只改变 generated TypeScript 类型来源，不改变事件 JSON、mapper 运行行为、持久化或安全策略；无需数据或配置重置。
- 验证：IPC contract generation/check、Rust workspace 编译与测试/Clippy、UI check/test/build、ADR 索引和 `git diff --check`。

## 回滚

从 generator 移除 `VadStatusEvent` 显式导出，并恢复 UI 本地接口即可；无数据、配置或 wire 迁移。
