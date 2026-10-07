# ADR 0647：为 Tauri listener 保留 unknown event payload 边界

## 状态

已采纳并实施。

## 背景

`tauri.ts::listen` 接收的 Tauri library callback 在本地 wrapper 中被擦除为 `unknown`；event registration 的 `registerListeners` 与 `registerOne` 却使用 `any` 作为 handler event type，以便将监听器直接传给它。真实 callback 是包含 `event`、`id` 与 `payload` 的 Tauri envelope；payload 本身仍是不可信 wire value，并由 session、Agent、ToolRun、recording 与 App domain mapper 验证。把整个 event 定成 `any` 会连 envelope 字段和 payload 都从类型系统里消失。

## 决定

1. Tauri listener handler 统一使用 `TauriEvent<unknown>`；payload 在领域 mapper 之前保持 `unknown`。
2. `tauri.ts::listen` 在 Tauri library callback 边界把 event envelope 投影为该类型；不在通用 listener registry 中对 payload 做宽泛 cast。
3. `registerListeners`、`registerOne` 和 handler maps 复用该 envelope 类型，domain payload validation 仍由现有 mapper 唯一完成。

## 替代方案

- 继续使用 `any` handler：拒绝。调用方可绕过 mapper 并任意读取 payload。
- 把 `TauriEvent<unknown>` payload 改为某一个 domain DTO：拒绝。一个 listener registry 承载多个不相同的 event contracts。
- 在通用 listener wrapper 校验 payload：拒绝。channel 到 DTO 的定义和运行时校验已归各 domain mapper 所有，不在 registration layer 重复维护。

## 影响与验证

只收窄内部监听 callback 的静态 envelope 类型；channel 注册、异常隔离、mapper 和 UI reducer 的运行行为不变。验证：`corepack pnpm run check`、`corepack pnpm run test:run`、ADR 索引与 staged diff 检查。

## 回滚

如需回滚，恢复 listener handler 的 `any` 类型，并同步撤回命名规范、路线图和 ADR 索引；无 Tauri wire 或持久化变化。
