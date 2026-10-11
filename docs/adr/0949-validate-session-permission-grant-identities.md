# 0949：校验 Session permission grant 的会话身份

## 状态

已接受并实现（2026-10-11）。

## 背景

`list_session_permissions` 返回由 SessionStore 持久化的授权 grant，包含所属 `session_id`。Settings command adapter 之前直接把 Tauri `invoke` 的运行时值作为生成 DTO 返回，UI 会使用该身份发起撤销操作。

## 决定

- 在 Settings contract 中校验 grant response 必须是数组，且每一行的 `session_id` 必须是规范 `ses-*`。
- 保留 Rust 生成的 `SessionPermissionGrant` DTO、合法 payload 与额外字段；不在此边界重复校验 grant 的其它领域字段。
- Settings command adapter 在返回 grant 前运行校验；格式错误的 response 以明确错误拒绝。

## 影响

IPC shape、授权语义、持久化与撤销行为不变。只阻止畸形 session identity 进入 UI，无需重置数据。

## 验证

- `corepack pnpm run check`
- `corepack pnpm run test:run`
- `corepack pnpm exec prettier --write` 涉及的 UI 文件
- `git diff --check`

## 回滚

若 grant 所属身份的权威类型发生变化，先更新 Rust DTO、统一 ID 规范与 parser 测试，再调整该边界；不要直接把未校验的 `invoke` 结果返回给 Settings 页面。
