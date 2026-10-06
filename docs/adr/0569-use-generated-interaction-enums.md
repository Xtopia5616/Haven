# ADR 0569：直接引用生成的交互分类类型

## 状态

已采纳并实施。

## 背景

Rust handler 生成的 `InteractionKind` 与 `InteractionStatus` 已是 IPC 分类的权威类型。`contracts/app.ts` 只用本地 type alias 原样 re-export 这两个 union；chat interaction helper、reducer 和菜单再从 App event contract 导入这些 aliases，模糊了 UI payload owner 与 wire enum owner 的关系。

## 决定

1. `contracts/app.ts` 直接 import generated `InteractionKind` / `InteractionStatus` 来定义其 renderer/wire payload shape，不对外导出同名 alias。
2. 其它 UI consumers 直接从 `generatedCommands.ts` 导入这两个 enum type。
3. 保留 App contract 对 `InteractionRequest`、camelCase `InteractionOwnerView`、owner 校验和 wire mapper 的所有权与行为。

## 替代方案

- 在每个 UI domain contract 重复 re-export 相同 union：拒绝。没有约束差异支持额外别名层。
- 把完整交互 event DTO 移到 generated/UI 混合模块：拒绝。generated shape 和 renderer owner/view 仍需分层映射。
- 合并 owner view 与 wire owner：拒绝。字段 casing 与路由/校验职责不同（ADR 0567）。

## 影响与验证

- 只改 TypeScript enum type 的 import owner；值集合、event contract、reducer 与 UI 行为不变。
- 验证：UI `check`、`test:run`、`build`，ADR 索引与差异空白检查通过。

## 回滚

恢复 `app.ts` 中的 `InteractionKind` / `InteractionStatus` aliases，并将 UI consumers 的 imports 改回 App contract。
