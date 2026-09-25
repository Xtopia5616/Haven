# ADR 0348：Action command contract boundary

- 状态：已采纳（2026-09-25）
- 范围：Action board UI 对 `list_actions` 与 `cancel_action` 的调用边界
- 关联：[ADR 0335](0335-action-board-ui-contract-mapper.md)、[ADR 0344](0344-action-lifecycle-ui-projection-boundary.md)

## 背景与审计

`commands.ts` 登记了全部 71 个 Tauri 命令，但实际调用仍经过返回 `any` 的通用 `invoke`。Action board 的 `actionStore` 直接调用 `list_actions`，随后复用 `mapActionPayload` 转换 Rust `ActionEvent`；`cancel_action` 则在 store 内手写 `{ actionId, kind }` 并把未定型结果交给调用方。Rust 的 `ActionEvent`、命令参数和返回类型均已有明确权威，且 `mapActionPayload` 已被 Action event listener 共用。

UI 当前没有调用 `list_action_history` 或 `delete_action`。本切片只审计并收口现有 Action board 调用链，不为无调用者命令新增 wrapper。

## 决定

1. 新增 `actionCommands.ts` 作为 Action board command boundary。`listActionRows` 把 `list_actions` 结果接收为 `unknown`，并对每一行调用唯一的 `mapActionPayload`；malformed 顶层继续返回 no-op 结果，malformed 行继续交给 store 记录不含 payload 的通用 warning 并跳过。
2. 在 `contracts/commands.ts` 定义扁平 wire 参数 `CancelActionRequest`，其 `kind` 复用 `ActionKind`。`cancelActionCommand` 以该 request 调用 `cancel_action`，并对 UI 暴露 `Promise<boolean>`。
3. `actionStore` 只负责 store 投影和 malformed-row warning，不再直接调用 `invoke` 或执行 wire 字段映射。`cancelAction` 保持既有公开签名和默认 kind。
4. 更新 IPC contract 检查脚本，确保 Action store 不直接调用这两个命令，list response 通过 mapper，并且 cancel request/result 使用命名类型。
5. 不修改 Rust command name、参数或返回值、IPC payload、DB、取消/错误语义、Action 投影或 UI 行为；不引入全局 codegen。

## 替代方案

- 让 `actionStore` 继续直接调用 Tauri 并内联消费 request/result：会保留 Action command 的无类型入口和 mapper 与 command 分离的所有权，拒绝。
- 为全部 71 个命令添加 typed wrapper：超出本切片的单域范围，且没有一个已验证的通用 runtime strategy，拒绝。
- 给没有 UI 调用者的 Action history/delete 命令建立新 wrapper：没有现有消费路径可验证，留待其 UI 使用前审计。

## 影响与验证

运行时 wire 行为不变。非数组 `list_actions` response 仍不更新 store；有效行映射、未知 status 降级、未知 kind 丢弃、warning、refresh generation/state-version gate 和取消错误传播保持不变。新增 command boundary 单元测试覆盖 snake_case 映射、malformed 行和顶层值、扁平取消请求、boolean 结果及取消 promise 原样转发。contract 脚本同时固定这些调用边界。

验收命令：

```sh
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
corepack pnpm --dir ui run build
pwsh -NoProfile -File scripts/check-ipc-events.ps1
pwsh -NoProfile -File scripts/check-ipc-contracts.ps1
git diff --check
```

本切片不改 Rust，因此不需要 Rust gate。

## 回滚

回滚本提交可将 Action store 恢复为直接调用 `list_actions`/`cancel_action`，并移除 command boundary、测试、contract 脚本断言、ADR 与 roadmap 更新。无需 Rust 修改、IPC 迁移、配置迁移或数据重置。
