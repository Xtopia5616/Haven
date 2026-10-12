# ADR 1048：对齐 UI canonical ID 校验

## 状态

已接受（2026-10-12）

## 背景

Rust `is_canonical_id` 要求 prefix 非空且只含 ASCII 小写字母和数字，ID 后缀必须恰为 32 位小写 hex。UI 的 `isCanonicalEntityId` 只检查 prefix 非空和 UUID 后缀正则，因而允许无效 prefix，并可能把带尾随换行的值当作 canonical ID。

## 决定

- UI 与 Rust 对 prefix 使用同一规则：`[a-z0-9]+`。
- UI 先检查完整 ID 字符长度，再校验固定前缀、分隔符和 32 位小写 hex 后缀，避免 JavaScript regex 行尾语义放宽边界。
- 非法 prefix 或额外尾随字符由 UI contract guard 拒绝。

## 影响与兼容

仅收紧 renderer 对 Haven 自有实体 ID 的校验。有效 wire 值不变；不影响 provider ID 或其它开放字符串。无持久化变化，不需要迁移或重置。

## 验证

- UI contract 测试覆盖空、大小写混合、带标点、下划线与非 ASCII prefix，以及带尾随换行的 ID。
- `corepack pnpm run check` 与 `corepack pnpm run test:run`。
