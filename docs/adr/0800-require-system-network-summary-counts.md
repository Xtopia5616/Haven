# ADR 0800：要求 System network summary 提供完整计数

## 状态

已采纳并实施（2026-10-08）。

## 背景

Rust `network_summary()` 对存在的 `network_summary` 对象总是输出 `interface_count`、`up_or_unknown` 与 `down`。UI 的 nested validator 和 `ToolSystemResult` props 却把三个字段都当成可选；模板将缺失值回退为 `0`，会把畸形 `ToolResult.output` 呈现成看似有效的统计。

## 决定

1. 当 `network_summary` 存在且非 null 时，三个计数必须都是有限数字；缺少任一字段或字段类型无效时由 renderer registry 回退 JsonView。
2. `ToolSystemResult` 的对应 nested Props 改为完整必需计数；summary 本身仍可缺省/null。
3. 加入缺少 `down` 的畸形 payload 回退用例，并用完整 producer shape 固定专用 renderer 路径。

## 影响与回滚

有效的 Rust 输出与呈现数值不变；畸形/不完整 summary 不再伪装为零计数。无 Rust wire、IPC、持久化或配置变化。回滚时恢复可选字段 guard/Props 与零值 fallback。

## 验收

运行 UI 类型检查与完整 UI 测试、ADR 索引检查；无 contract generator 影响。
