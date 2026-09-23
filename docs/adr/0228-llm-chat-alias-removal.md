# ADR 0228：删除 LLM router chat 转发别名

- 状态：已采纳（2026-09-24）
- 范围：`haven-llm` 的 `LlmRouter` chat 请求入口
- 关联：[ADR 0221](0221-chat-request-policy-snapshot.md)、[ADR 0052](0052-remove-expired-compatibility-layers.md)

## 背景

`LlmRouter::chat` 和 `LlmRouter::chat_with_output_cap` 只转发到 request-shaped 方法，没有自己的策略或执行职责。它们使调用者同时面对两套等价入口，也让 router 的真实请求边界不清晰。

## 决定

1. 删除 `LlmRouter::chat`，调用方使用 `chat_request`。
2. 删除 `LlmRouter::chat_with_output_cap`，调用方使用 `chat_request_with_output_cap`。
3. `chat_with_prompt` 与可取消 chat 直接调用保留的 request-shaped 入口；provider 层的 `LlmClient::chat_with_output_cap` 不变。
4. 保持 `execute_chat_request`、permit 后单次 `RequestPolicy` 快照、provider 参数、usage、重试、超时和错误语义不变。
5. 本仓库内的 Rust 调用点一并迁移；不为仓库外调用者保留过渡别名。若未来需要稳定外部 API，应另行设计版本化 facade。

## 替代方案

- 保留两个别名：会继续维护重复的公共入口，违背删除过期兼容层的目标。
- 现在同时改造 provider adapter：会扩大写集并混淆 router API 收口与 wire mapping，拒绝。

## 影响与验证

router 公共 Rust API 发生破坏性收缩，但不改变 provider wire、配置、数据库或 IPC 契约。已验证：`haven-llm` 单元测试、严格 clippy、全 workspace check，以及 `file_summary` 调用点迁移。

## 回滚

若有未迁移的仓库内调用方，恢复两个薄别名并保留 request-shaped 实现；不需要数据重置。
