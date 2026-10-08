# ADR 0782：对齐 Memory renderer 嵌套 Props 的 null 语义

## 状态

已采纳并实施（2026-10-08）。

## 背景

Memory ToolResult 的 facts、hits、stored 与 root 字段经动态 JSON 进入 UI。registry 的 optional-field guard 将 `null` 当作缺省值，但 `MemoryFact`、`MemoryHit` 与 `ToolMemoryResult.Props` 只对少数字段表达 nullable。静态嵌套类型因而窄于 renderer 实际接受的形状。

## 决定

1. 对齐 `MemoryFact`、`MemoryHit`、stored record 与 root Props：guard 接受 null 的可选字段都在 alias 中显式声明 `| null`。
2. 保持 facts/hits array 与对象字段本身的 record/array 结构要求；非 null 的 tags、score、confidence 等仍按原类型校验。
3. 用 renderer contract 覆盖可选字段全为 null 的有效 row 与错误 tags 类型的 fallback。

## 影响与回滚

仅扩大动态输出边界的静态 Props，使之准确反映 guard 对 null 的缺省处理。无需修改 Rust producer、IPC、存储或界面行为；guard 改为拒绝相应 null 后可同步收窄 alias 回滚。

## 验收

运行 Svelte type check、UI 全量测试与 ADR 索引检查。
