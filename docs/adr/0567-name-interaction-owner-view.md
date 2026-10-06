# ADR 0567：区分交互 owner wire 与 renderer view

## 状态

已采纳并实施。

## 背景

Rust 生成的 `InteractionOwner` 是 App event / confirmation command 的 snake_case wire union（`session_id` / `tool_run_id`）。`contracts/app.ts` 又定义同名 camelCase union，mapper 将不可信 wire owner 校验并投影为 renderer routing data，回发时由 `interactionOwnerToWire` 转回 wire shape。二者是不同边界形状，generated type 已在本地导入为 `InteractionOwnerWire`，但 exported renderer type 名未标出角色。

## 决定

1. 将 App 的 renderer union 改名为 `InteractionOwnerView`；generated `InteractionOwner` 继续作为 wire authority。
2. interaction request mapper、discriminated request variants 和 `interactionOwnerToWire` 都使用 view 类型。
3. 保持 owner 校验、session 关联、request id、命令序列化和 lifecycle 行为不变。

## 替代方案

- 合并 wire 与 renderer owner 类型：拒绝。字段 casing 和 mapper 的校验/关联规则属于不同边界。
- 保留 renderer 侧泛称 `InteractionOwner`：拒绝。跨模块符号检索无法识别它不是 generated wire shape。
- 改用 `InteractionOwnerDto`：拒绝。该类型不是新的传输 DTO，而是已映射的 UI view。

## 影响与验证

- 只改 UI renderer contract 类型名及当前 roadmap/naming 说明；IPC shape 和 command payload 不变。
- 验证：UI `check`、`test:run`、`build`，ADR 索引与差异空白检查通过。

## 回滚

将 App contract renderer union 恢复为 `InteractionOwner`，并将 roadmap/naming 中的 view 说明恢复为原类型名。
