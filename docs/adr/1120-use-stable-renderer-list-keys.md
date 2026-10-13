# ADR 1120：为 renderer 列表使用稳定且无碰撞的身份键

## 状态

已接受（2026-10-13）

## 背景

若 Svelte keyed each 使用可变列表位置或直接拼接多个字段生成 key，不同结果可能共享 DOM 身份；删除前项也会改变后续 Composer 附件的 key。文件/网页搜索使用路径、行号、URL 与标题等多字段组合；Composer 草稿附件在提交前没有持久实体 ID。

## 决定

- 搜索 renderer 使用语义字段的无歧义 tuple 编码作为 key；Web citation 再包含同组出现序号，以区分完全重复的返回项。
- Composer 图片与文件在加入草稿时分配仅用于 renderer 的 `draft-attachment-*` 身份；移除按该身份定位，提交时只投影稳定 payload 字段，不泄漏本地 key。
- 不改变工具结果、IPC、附件持久身份或分页排序。

## 替代方案

- 使用 `fieldA + fieldB`：字段边界含糊，可能使不同结果碰撞。
- 使用列表下标：前项删除或排序变化会改变后续身份。
- 将附件内容或文件名作为 key：相同附件会碰撞，图片 key 还会复制整段 base64 到临时 DOM identity。

## 影响与验证

- 仅改变 renderer reconciliation；无持久化、配置、IPC 或数据库变更，无需重置。
- UI 回归测试覆盖跨字段拼接碰撞、重复网页 citation、同名文件、相同图片 payload、移除前项后节点身份稳定及提交 payload 不带草稿 key。
- 验证：`corepack pnpm run check`、`corepack pnpm run test:run`。

## 回滚

回滚时同时移除无碰撞 search key、草稿附件本地 key、相关测试和本 ADR；不涉及数据迁移。
