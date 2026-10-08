# ADR 0783：要求 HTTP renderer 的响应状态

## 状态

已采纳并实施（2026-10-08）。

## 背景

HTTP 成功 producer 始终输出数值 `status`，但 UI guard 将其当作可选字段，专用 renderer 又无条件把该值转成 badge 文本。因此空对象也会进入 HTTP renderer 并显示 `undefined` 状态。

## 决定

1. HTTP builtin shape guard 要求 root `status` 是有限数值；缺失、null 或错误类型回退通用 JSON renderer。
2. `ToolHttpResult` 对缺失 status 安全地省略 badge；共享动态 renderer 调用仍传递开放记录，因此组件 Props 保持可选输入。
3. `truncated` 与 `body` 保持原 optional contract；取消/失败结果的 null output 继续走非专用 JSON/错误路径。

## 影响与回滚

仅收紧 UI 成功响应展示契约并避免空状态 badge；HTTP 请求和 ToolResult wire 不变。可通过将 guard 恢复为 optional 并还原无条件 badge 回滚。

## 验收

Renderer contract tests 覆盖 status 缺失/null fallback、有效 status renderer selection 与响应类型错误；运行 Svelte type check、UI 全量测试与 ADR 索引检查。
