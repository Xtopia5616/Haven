# ADR 1058：收窄 LLM crate 根 API 并归还能力判断 owner

## 状态

已接受（2026-10-12）

## 背景

provider `api_style` 的规范化与能力谓词由 Common 定义，供配置物化和运行时判断共用；`haven-llm` 曾经再次从 crate root 导出这些 helper，App 因而通过 LLM crate 调用 Common 所有的能力判断。LLM root 还导出了只供对应 builder 使用的运行时配置解析函数、携带 API key 的 `Resolved*Config`，以及只在 LLM 内部消费的 context-window helper。

## 决定

- Common 继续唯一拥有 provider wire-style 规范化和能力谓词。App model command 与 `LlmRouter` 直接从 `haven_common::config` 导入这些 helper；LLM 不再重导出 Common helper。
- `parse_web_search_mode` 保持在 LLM capability 实现内使用，不再导出到 crate root 或 adapter 外层。
- `resolve_image_gen_config`、`resolve_stt_config`、`resolve_tts_config` 及其 resolved runtime 配置留在各自媒体模块内；配置含 API key，不作为跨 crate 根 API 暴露。STT resolved config 不传播由 Tools 消费的 `min_confidence`；对应具体 provider client 的构造也留在模块内。
- `context_window_for` 仅在 LLM crate 内可见；跨 crate 调用者使用 `LlmRouter::context_window_for_request`。
- 根 API 保留实际跨 crate 使用的 builder、client traits、`WebSearchMode` 和 `web_search_result_of`。

## 影响

provider capability 判断仍使用相同 Common 实现，媒体 builder 与路由行为不变；只缩小 Rust 可见 API，不改 IPC、wire 或持久化契约。现有跨 crate consumers 已迁到 canonical owner。

## 验证

- workspace check、Clippy 与全量 Rust 测试通过。
- IPC handler / event 契约检查及 ADR 索引检查通过。
