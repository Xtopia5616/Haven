# ADR 0250：删除无调用的 prompt output-cap wrapper

- 状态：已采纳（2026-09-24）
- 范围：`haven-llm` 的 `LlmRouter` Rust API
- 关联：[ADR 0234](0234-llm-complete-request-object.md)

## 背景

`LlmRouter::chat_with_prompt_output_cap` 只负责把 System/User prompt 转为 `CompleteRequest`，并将可选输出上限传给 `complete`。`CompleteRequest` 已是唯一非流式请求入口，且直接承载 `max_output_tokens`。全仓搜索只找到该方法定义和 ADR 0234 的旧保留说明；没有 Rust 调用、测试、IPC/serde 字段或动态工具/插件名称引用。

## 决定

1. 删除 `chat_with_prompt_output_cap`。无需迁移仓库调用点，因为没有调用者。
2. 保留仍有调用的 `chat_with_prompt`，以及具有独立取消语义的 `chat_messages_cancellable`。
3. 需要 prompt 与输出上限的后续调用直接构造 `CompleteRequest`，通过 `max_output_tokens` 表达上限。
4. 同步修正 ADR 0234 对该 wrapper 的过时保留记录。

## 替代方案

- 保留该 wrapper：会继续暴露一个无调用的 DTO 转发入口；拒绝。
- 删除 `CompleteRequest::max_output_tokens`：会改变请求能力，不符合本切片范围；拒绝。

## 影响与验证

这是 `haven-llm` 的内部 Rust API 收缩。仓库内无调用点迁移；请求构造、输出上限透传、provider 行为、serde、IPC、数据库和用户数据均不变。没有对应测试引用需要删除。验证包括全仓符号搜索、workspace 编译、`haven-llm` 测试、格式检查与严格 workspace Clippy。

## 回滚

回退本切片提交即可恢复该方法和 ADR 0234 的旧记录；无数据重置要求。
