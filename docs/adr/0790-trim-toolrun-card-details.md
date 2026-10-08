# ADR 0790：收窄 ToolRun card 的详情投影

## 状态

已采纳并实施（2026-10-08）。

## 背景

`ToolRunCardDetails` 保存 background / scheduled ToolRun 的详情子集。复核 `ToolRunCenter` 的全部生产读取点后，发现 `dueAt`、`title`、`mode`、`errorReason` 与 `exitCode` 只在投影和测试中赋值，没有 UI 消费者；对应信息已经由卡片的 `title`、`timing` 或其它生命周期契约表达，留在 `details` 会扩大本地 projection 类型而不提供展示行为。

## 决定

从 `ToolRunCardDetails` 与 projection 移除上述五个未消费字段。保留生产 UI 用于搜索、摘要与详情展示的 `command`、`output`、`error`、`preview`、`body`。

## 影响与回滚

只收窄 UI 内部派生 view，不改 ToolRun IPC、持久化行或工具输出。若之后出现明确的消费者，再依据该消费者恢复字段及其 owner 类型。

## 验收

复核 `ToolRunCenter` 与整个 UI 对 `details.*` 的读取点，并运行 Svelte type check 和 UI 全量测试。
