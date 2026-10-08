# ADR 0784：校验非空剪贴板历史的 total

## 状态

已采纳并实施（2026-10-08）。

## 背景

`ClipboardTool` 的 history producer 对每条结果都输出 `entries` 与 `total`。当 `entries` 非空时，`ToolClipboardResult` 会无条件展示 `total`；但 nested guard 只验证 total（若有），因此缺失/null total 仍会进入 renderer 并显示空计数。空 history 不会读取 total。

## 决定

1. `entries` 非空时要求 `total` 是有限数值；缺失、null 或错误类型回退通用 JSON renderer。
2. 空 entries 或不含 entries 的 clipboard 输出继续允许 optional total。
3. Props 明示 `written`、`content`、`total` 的 guard 接受 null 语义；entry row 仍只校验 renderer 实际读取的必需 `content`，忽略 timestamp 等未消费字段。

## 影响与回滚

只收紧动态输出展示 guard，producer 与历史保存行为不变。若 future producer 允许缺少 total，应同步让 renderer 安全省略计数并调整该条件 guard。

## 验收

Renderer contract tests 覆盖非空 entries 缺少 total 的 fallback、空列表和合法历史的专用 renderer selection，以及未消费 row 字段不触发降级；运行 Svelte type check、UI 全量测试与 ADR 索引检查。
