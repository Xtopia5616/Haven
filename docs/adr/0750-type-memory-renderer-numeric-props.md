# ADR 0750：Memory renderer 数值 props 对齐嵌套校验

## 状态

已采纳并实施。

## 背景

Memory builtin renderer registry 已要求 `facts[].confidence` 与 `hits[].score` 是有限数值；字段缺失或 `null` 表示不显示分数。`ToolMemoryResult` 却将两字段声明为 `unknown`，`scoreLabel` 再用 `Number(value)` 将字符串等输入强制转换，和边界校验及 renderer view shape 不一致。

## 决定

- renderer row props 中两个分数字段使用 `number | null` 可选形状，与 validator 对 null 缺省的处理相同。
- `scoreLabel` 接受 `number` 并只负责显示格式，不再进行字符串/其它值的转换。
- 畸形值仍由 registry 拒绝专用 renderer 并显示原始 JSON。

## 影响与验证

只对齐 UI builtin renderer 内部类型，不改变 Rust producer、动态 ToolResult JSON、provider wire 或历史记录。负向用例覆盖字符串 confidence/score；通过 Svelte 类型检查与 UI 全量测试验证。

## 回滚

可恢复 `unknown` props 和宽泛数值转换。没有数据或外部契约迁移。
