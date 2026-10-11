# ADR 0989：Provider 与 MCP 日志只记录稳定错误类别

## 状态

已采纳并实施。

## 背景

ADR 0987 收紧了用户正文、工具输出、协议元数据和路径的日志范围，但 LLM Provider 与 MCP 的部分失败路径仍将经 `sanitize_error_text` 处理后的外部错误写入日志。外部错误可能包含请求片段、服务器返回文本或本机路径；清洗不能保证这些内容适合留在运维日志中。

## 决定

1. Provider 错误日志记录稳定的 `LlmConnectionFailureReason`，MCP 错误日志记录固定事件描述及受控服务器身份，不记录外部错误原文或清洗后的错误文本。
2. SSE UTF-8/JSON 解析日志只记录被忽略的行或数据字节数。
3. 错误对象仍沿原有 `Result`、连接状态和事件路径传递。本 ADR 只统一日志内容，不改变重试、错误分类和用户可见错误。

## 影响与验证

- LLM retry、adapter 初始化、stream 请求、Gemini 缓存回退及连接探测日志使用稳定错误类别。
- MCP 连接、工具刷新、shutdown、child cleanup 和 SSE listener 日志使用固定描述；解析器日志仅记录字节数。
- 复核 LLM/MCP 源码中 tracing 调用并运行 workspace 测试、严格 Clippy 与相关契约检查。

## 回滚

恢复外部错误文本日志即可；无需数据或配置迁移。回滚会重新将不受控 Provider/MCP 内容写入日志。
