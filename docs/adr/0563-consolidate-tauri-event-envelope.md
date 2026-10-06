# ADR 0563：合并 Tauri 事件通用 envelope 类型

## 状态

已采纳并实施。

## 背景

`contracts/session.ts` 与 `contracts/toolRun.ts` 各自声明了完全相同的 `TauriEvent<T>`：Tauri channel 名 `event`、事件序号 `id` 和领域 `payload`。Agent、App、录音 contracts 及 chat event listeners 也依赖该通用 shape，其中多个模块此前绕道从 Session contract 导入，导致通用 Tauri envelope 看似归 Session 所有。

## 决定

1. 在 `contracts/tauriEvent.ts` 集中定义唯一通用 `TauriEvent<T>`。
2. Session、ToolRun、Agent、App、录音 contracts 和 listener consumers 都从该通用 owner 导入 envelope；每个领域 contract 继续单独拥有自身 payload 和 validator。
3. 不修改 Tauri listener、channel 名、事件序号、payload、mapper 校验或运行行为。

## 替代方案

- 在 Session contract 保留一个定义供所有 domain 借用：拒绝。通用平台 envelope 不属于 Session 生命周期。
- 保留 Session 与 ToolRun 两份声明：拒绝。形状与语义完全相同，没有不同生命周期或校验约束支持双 owner。
- 把各 domain payload 合并成一个总事件 DTO：拒绝。领域 payload、channel registry 与 fail-closed mapper 仍由各自 contract 拥有。

## 影响与验证

- 只调整前端 TypeScript 类型 owner 与导入路径；无 IPC、事件或 UI 行为变化。
- 验证：UI `check`、`test:run`、`build`，ADR 索引与差异空白检查通过。

## 回滚

恢复 `session.ts` 和 `toolRun.ts` 中的本地 `TauriEvent<T>` 声明，并将消费者导入改回各领域 contract。
