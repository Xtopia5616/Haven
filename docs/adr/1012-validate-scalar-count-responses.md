# 1012：校验标量计数响应的 JavaScript 整数精度

## 状态

已采纳并实施（2026-10-12）。

## 背景

Metrics command 已拒绝 JavaScript 无法精确表示的 Rust `u64`。其余 count command 仍把 `u64`、`usize` 或 SQL `i64` 直接作为 JavaScript `number` 返回；这既可静默失真，也可把负数或非整数当作有效计数。Session history 的两个 count handler 尚无 renderer adapter。

## 决定

- 所有 scalar count command adapter 统一使用 `parseCountResponse`，只接受非负 JavaScript safe integer。
- Session history 总数和搜索总数新增命名 adapter；调用方通过该 adapter 使用 count command。
- Rust command、数据库结果和 IPC JSON 字段保持不变；不安全数值视为 command failure。

## 影响

不改变 Rust handler、生成类型、持久化数据或数据库 schema。实际计数不可能被 renderer 精确表示时，调用失败，而不是向 UI 传递被舍入的结果。无需数据库重置。

## 替代方案

- 继续直接透传：拒绝；极大整数会失真，负值和错误类型也会泄漏到 count 消费者。
- 将所有 count 改为十进制字符串：拒绝；会破坏现有命令类型和消费者，且当前无需支持超安全整数展示。

## 验证

- `corepack pnpm exec vitest run src/lib/countCommands.test.ts src/lib/contracts/count.test.ts`
- `corepack pnpm run check`
- `pwsh -NoProfile -File scripts/check-ipc-contracts.ps1`
- 完整 UI 测试与构建在本轮全量验收中复跑。

## 回滚

移除 count parser 并恢复原始 invoke promise 即可回滚；这会重新开放不安全整数和畸形标量计数。
