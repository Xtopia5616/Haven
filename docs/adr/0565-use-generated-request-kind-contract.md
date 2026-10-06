# ADR 0565：以生成契约统一 RequestKind 枚举来源

## 状态

已采纳并实施。

## 背景

Rust `haven_common::config::RequestKind` 是 IPC 与路由配置的权威枚举，generated TypeScript 已提供 `RequestKind` 和 `REQUEST_KIND_VALUES`。UI `modelRoles.ts` 再手写了一份同值 `requestKindValues` / `RequestKind`，并重复列出 request option 的 value；Agent event contract 从 UI 展示模块导入这个类型。

## 决定

1. `requestPolicyOptions` 按 generated `REQUEST_KIND_VALUES` 的权威顺序生成；本地只保留穷尽检查的 `RequestKind` 标签映射。
2. 删除 UI 本地 request kind 值数组和重复 union；Agent event contract 直接使用 generated `RequestKind`。
3. 保持所有值、标签、顺序、路由策略和事件 payload 不变。

## 替代方案

- 继续双份维护 union 与数组：拒绝。值已由 Rust enum 与生成契约拥有，重复列表会随新增/删除 drift。
- 改变前端选项顺序：拒绝。当前 generated 值顺序与 UI 顺序一致，按生成值派生即可保留。
- 让 generated contract 依赖 UI model-role 模块：拒绝。依赖方向应由 UI 消费 Rust 契约，而不是反向。

## 影响与验证

- 只改 UI enum/value owner 和 option assembly；IPC shape、用户标签与运行行为不变。
- 验证：UI `check`、`test:run`、`build`，ADR 索引与差异空白检查通过。

## 回滚

恢复 `modelRoles.ts` 的 `requestKindValues` / `RequestKind` 本地 union，并恢复 `agent.ts` 对 UI 模块的类型依赖。
