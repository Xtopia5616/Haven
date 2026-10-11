# ADR 0980：Agent 事件数值边界匹配 Rust wire 类型

## 状态

已采纳并实施。

## 背景

Agent event mapper 曾将所有序号、计数和时长统一按有限 JavaScript number 接受。Rust wire DTO 对这些字段实际声明为 `u32` 或 `u64`，所以 UI 会接收负数、小数以及精度超出 JavaScript 安全整数范围的值；后续排序、步骤关联或计量会使用失真的数据。

## 决定

1. Rust `u32` 对应字段只接受整数 `0..=4_294_967_295`，包括步骤号、工具索引、token 计数和 context window。
2. Rust `u64` 对应字段在 JSON number 边界只接受 JavaScript 安全范围内的非负整数（`0..=Number.MAX_SAFE_INTEGER`），包括 run/event/chunk 序号和毫秒时长。更大的 JSON number 无法在 JavaScript 中精确保留，因此事件 mapper 失败关闭。
3. Tauri event envelope 的 `id` 也要求非负安全整数。金额等 Rust 浮点字段仍只要求有限数值，不套用整数约束。

## 替代方案

- 所有数字继续只检查有限性：拒绝；不能保证运行时值符合 Rust wire 类型。
- 把 `u64` 事件字段改成十进制字符串或 BigInt：暂不采用；会改变既有 IPC shape，当前计数语义和生命周期无需此变更。

## 影响与验证

- Agent event UI mapper 按 Rust primitive range 验证整数，非法事件继续被忽略，不会进入会话投影。
- 测试覆盖负数、小数、u32 溢出、安全整数溢出及有效上界。
- Rust DTO、IPC 字段名、数据库和事件发布行为不变；此前能被错误接受的畸形数字现在被拒绝。

## 回滚

回退 Agent mapper 的整数 guard 和对应测试，并同步回退命名规范、路线图与本 ADR 索引；无需数据迁移。
