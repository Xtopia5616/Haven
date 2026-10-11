# ADR 0986：统一设置数值的 wire 与领域边界校验

## 状态

已采纳并实施。

## 背景

`Settings` 将 Rust `u64`、`i64` 与 Windows `usize` 序列化为 JavaScript `number`。虽然 `i128/u128` 已被 IPC 生成器拒绝，64 位整数仍可能超出 `Number.MAX_SAFE_INTEGER`；设置页读取后再整体保存会把原值舍入。浮点字段也可能从 TOML 或 IPC 收到非有限值或超出其声明范围的值。设置控件的输入 min/max 不约束初始载入值。

## 决定

1. `haven_common::config` 是数值验证的权威 owner。AppConfig 与 Settings 的全部序列化整数递归校验为 JavaScript 可精确往返的安全整数；本机保存、配置载入和 Settings 写入共用该校验。
2. 比例、置信度、温度和重试抖动按其字段契约验证为有限值并在有效区间内；模型 token 价格必须是有限非负值；定时任务最远排期不得为负。
3. 无效 TOML 数值沿用既有损坏/语义无效配置流程：保留带时间戳备份并载入默认配置。ConfigLoader 保存、ConfigService 构造/patch 和 `update_settings` IPC 对无效值失败关闭；patch 失败不改变当前快照或版本。
4. UI 在解析 Settings 响应和提交 Settings 请求时递归拒绝非有限数及不安全整数。Common 仍是最终权威校验者。
5. `default_context_window` 在 UI 的 K-token 输入上限为 `floor(u32::MAX / 1000)`，避免控件接受无法解码为 Rust `u32` 的值。
6. 不改变 IPC 数字 wire type、配置字段名或数据库；无需数据库迁移。已有超界 TOML 会被备份并回默认，用户可从备份取回并手工修正。

## 影响与验证

- `Number.MAX_SAFE_INTEGER` 与 `u32::MAX` 边界、超界整数、负排期、非有限浮点与越界比例/温度均有 Rust 覆盖。
- UI 覆盖 Settings 读入与写出拒绝不安全数值。
- 配置载入对超界整数保留原文件备份；ConfigService 拒绝 patch 且保持快照不变。
- 完成验证：`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`corepack pnpm run check`、`corepack pnpm run test:run`、Prettier。

## 回滚

移除 Common 数值校验和 UI safe-number 检查，并恢复 `default_context_window` 的通用控件上限。若回滚且配置文件仍包含被新规则拒绝的值，恢复校验后须从 timestamped backup 手工修正。无需数据库重置。
