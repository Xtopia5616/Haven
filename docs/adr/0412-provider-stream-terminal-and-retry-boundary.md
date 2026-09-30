# ADR 0412：Provider 流终止事件与重试边界

- 状态：已采纳
- 日期：2026-09-30
- 范围：OpenAI Chat Completions、Anthropic Messages、OpenAI Responses、Gemini 流适配器与聚合重试器

## 背景

传输层 EOF 不代表 provider 响应已按协议完成。部分适配器在参数 JSON 看起来完整时仍会于 EOF flush 工具调用；Anthropic 还会在 `message_stop` 前交付已结束的 `tool_use` 块。Agent 只能在完整聚合结果成功后执行工具，但提前交付的工具内容仍会污染流回调与重试判断。

聚合重试器此前把任何回调都当作已有输出。Anthropic 的 `message_start` 可以只包含模型与用量 metadata，因此后续在尚无实际内容时发生的可重试错误也会被抑制重试。

## 决定

- 工具调用只在对应协议终止事件已收到后交付：OpenAI Chat Completions 使用 `finish_reason`，Anthropic 使用 `message_stop`，OpenAI Responses 使用 `response.completed` 或 `response.incomplete`，Gemini 使用 candidate `finishReason`。
- 若传输 EOF 前未收到对应终止事件，流以 `StreamTruncated` 失败；不因参数看起来完整、响应无可见内容或只收到 metadata 而接受 EOF。
- Anthropic 在 `content_block_stop` 暂存工具调用，直到 `message_stop` 才随终态 chunk 一起交付。
- 重试门槛按已交付的实际内容判断：非空文本、reasoning、工具调用或 web-search 输出会抑制重试。模型名、用量、finish reason 与 provider 的 opaque echo metadata 不会抑制重试。

## 替代方案

- 仅在 EOF 时检查参数是否为完整 JSON：无法确认 provider 是否完成生成，仍可能执行意外截断的动作。
- 将所有回调都视为输出：metadata-only 前缀会关闭安全重试窗口。
- 在消息块结束时立即交付 Anthropic 工具调用：传输可能在整个 assistant 消息结束前断开。

## 影响

缺少协议终止事件的流现在会作为截断失败并可按既有重试策略处理。正常终止响应的聚合结果保持不变；Anthropic 工具调用在收到 `message_stop` 后交付。没有数据库、配置或 IPC 契约变化，无需重置用户数据。

## 验证

- 四个 provider 各有回归测试：工具参数完整但缺少终止事件时返回 `StreamTruncated`，不交付工具调用。
- 聚合重试测试覆盖 metadata-only chunk 后发生可重试错误，并确认下一次尝试成功。
- 运行 LLM crate 测试及适用的 workspace 格式、编译和严格 Clippy 检查。

## 回滚与重置

回滚本 ADR 对应的终止事件检查、Anthropic 工具调用暂存与重试内容分类即可恢复原行为；无需重置用户数据。
