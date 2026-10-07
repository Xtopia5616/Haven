# ADR 0695：交互事件复用生成 wire 类型

## 状态

已采纳并实施。

## 背景

Rust `events.rs::InteractionRequestedEvent` 已进入生成的 Tauri DTO，因为 session resume response 也返回同一 projection。UI `contracts/app.ts::AppWirePayloadMap['interaction:requested']` 又手写了相同的 ID、owner union、状态、可选字段和 snake_case 名称；该 wire map 只供运行时验证后的投影读取字段。UI 的 `InteractionRequest` 则是 camelCase renderer view，包含经校验的 owner 关联、归一化 options 和动态 response，职责不同。

## 决定

- `AppWirePayloadMap['interaction:requested']` 直接引用 generated `InteractionRequestedEvent`。
- 保留 wire 值运行时校验、owner/session 关联检查和 `InteractionRequest` renderer view。
- 其它 App event 的本地 wire map 字段不在本切片范围内。

## 替代方案

- 保留手写 wire interface：拒绝。它与 generated DTO 完全重复，无法提供额外约束，且两份字段清单可能漂移。
- 让 handler 直接消费 generated DTO：拒绝。事件入口仍接收不可信的 Tauri payload，必须经过当前 mapper 的运行时校验与 snake_case 到 camelCase 投影。

## 影响与验证

- 只调整前端 wire 类型来源，不改变 event JSON、renderer shape、持久化或安全行为；无需数据或配置重置。
- 验证：`corepack pnpm run check`、`corepack pnpm run test:run`、ADR 索引检查和 `git diff --check`。

## 回滚

恢复 `AppWirePayloadMap['interaction:requested']` 的本地字段声明即可；无数据或 wire 迁移。
