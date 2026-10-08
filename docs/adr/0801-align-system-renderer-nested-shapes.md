# ADR 0801：对齐 System renderer nested shapes

## 状态

已采纳并实施（2026-10-08）。

## 背景

System producer 对存在的 CPU 对象总是输出 `cores` 与 `logical_cpus`，唯独 overview 会省略 `usage_pct`。对存在的 memory 对象总是输出 `used_bytes`、`total_bytes` 与 `available_bytes`。磁盘行总是带有 `mount`、`total_bytes`、`available_bytes`；网络行总是带有 `name`、`state`、`ips`；Windows 显示器行带有 `name`、`width`、`height`、`primary`，renderer 用 `name` 作为稳定 key。非 Windows producer 则返回 `{ available: false, note }` 占位行。此前 nested guard 允许 renderer 读取字段缺失，CPU count 还回退到 0，磁盘算术则会使用不完整值。

## 决定

1. 存在的 CPU 对象必须包含有限 `cores` 与 `logical_cpus`；`usage_pct` 仍可缺省/null，因为 overview 明确不采样。
2. 存在的 memory 对象必须包含有限 `used_bytes` 与 `total_bytes`；不校验专用 renderer 不消费的 memory `available_bytes`。
3. 磁盘、网络和 Windows 显示器数组行必须包含 renderer 实际读取的 producer 字段；Props 与 guard 均体现这些必需字段，显示器 key 直接使用必需的 `name`。非 Windows display sentinel 作为独立联合成员保留，并展示 `note`。
4. 畸形已消费字段回退 JsonView；测试覆盖缺失计数/字节/行字段和未消费字段畸形时保留专用 renderer。

## 影响与回滚

Rust producer 输出与 wire shape 不变；完整输出保持相同渲染。畸形对象不再显示合成的零计数、空 size、缺失网卡行或 `undefined` 分辨率；非 Windows producer 继续显示不可用说明。未消费的额外字段仍作为 ToolResult JSON 传递，不从结果剔除。无 IPC、持久化或配置迁移。

## 验收

运行 UI 类型检查与完整 UI 测试、ADR 索引检查；无 contract generator 影响。
