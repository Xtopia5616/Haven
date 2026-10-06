# ADR 0588：为 Ask 与通知信号使用具名结果

## 状态

已采纳并实施。

## 背景

Tools 从工具结构化输出中解析两种 side-channel。Ask helper 返回 `(question, options)`，通知 helper 返回 `(title, body)`，且两个通知字段同时为 `None` 表示没有通知。tuple 位置令消费者需要记住问题、建议选项、通知标题与正文各自所处的位置；通知的双 `Option` 还允许表达内部不一致的组合。

## 决定

1. `extract_ask_signal` 返回 `AskSignal { question, options }`，保留 question 可选、options 即使缺少 question 也照常解析的既有语义。
2. `extract_notify_signal` 返回 `Option<NotificationSignal { title, body }>`；未请求通知时为 `None`，请求时标题使用既有默认值 `Haven`，正文沿用既有空字符串缺省。
3. `ToolSignals` 的现有字段及其到 Agent 的传递形状保持不变；解析结果在 Tools builtin adapter 中按具名字段转换。

## 替代方案

- 仅在局部变量中给 tuple 成员起名：拒绝，helper API 仍隐藏字段语义，通知仍能产生不一致的 optional pair。
- 将 Ask 和通知合成一个统一信号枚举：拒绝，二者可独立声明，现有汇总 `ToolSignals` 是另一层 side-channel owner。

## 影响与验证

- 更新 Tools helper、公共导出、Ask/Notify adapter 与 helper 单测。
- 不改变 `ToolSignals` 序列化形状、事件、IPC、工具 JSON、暂停/通知行为或默认文案；不影响持久数据，无需重置。
- 验证通过：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked -- --test-threads=1`、ADR 索引与 `git diff --check`。

## 回滚

恢复两个 helper 的 tuple 返回签名与 Ask/Notify 调用点的 tuple 解构；`ToolSignals` 及其消费链无需变化。
