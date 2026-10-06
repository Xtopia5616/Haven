# ADR 0568：复用 generated RiskLevel 契约

## 状态

已采纳并实施。

## 背景

Rust `haven_common::types::RiskLevel` 生成了 TypeScript `RiskLevel`；App event contract 又手写完全相同的 `safe/low/medium/high/critical` union，并将其用于 camelCase interaction view 和 wire payload。`ConfirmationDialog` 从 App event contract 导入这个 union。风险枚举因而看起来由 UI App event 层定义。

## 决定

1. 删除 App contract 的本地 `RiskLevel` union，直接使用 generated `RiskLevel`。
2. `ConfirmationDialog` 直接引用 generated type；App interaction contract 继续自行负责字段 casing 与 owner mapper。
3. 保持允许值、IPC 字段、风险判定和对话框表现不变。

## 替代方案

- 保留 UI 手写 union：拒绝。它与安全领域 Rust enum 完全一致，会产生第二个枚举来源。
- 把完整 interaction wire DTO 直接交给对话框：拒绝。wire casing、owner 校验和 UI routing 仍属于 App contract 的 mapper。
- 在 generated 文件外再建一个 frontend risk union：拒绝。枚举值由 Rust 领域类型拥有，UI 不应复制。

## 影响与验证

- 只改 UI enum 类型来源；risk 值、协议、校验和展示行为不变。
- 验证：UI `check`、`test:run`、`build`，ADR 索引与差异空白检查通过。

## 回滚

恢复 `app.ts` 的本地 `RiskLevel` union，并将 `ConfirmationDialog` 的类型导入改回 App contract。
