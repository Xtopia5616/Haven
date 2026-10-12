# 1035：在 Rust API 中标明领域身份与配置 owner

## 状态

已采纳并实施（2026-10-12）。

## 背景

全库命名复核发现多处内部或跨 crate API 省略了实体词或把不同配置概念统称为 `model`：SessionActor handle 的 `id`、Memory fact-extraction transcript/projection 名称、Messaging 按消息身份查询的参数、scheduled ToolRun 局部身份变量，以及 Common/LLM 的模型配置读写 API。

## 决定

- Actor handle 身份使用 `session_id`；Memory 派生输入集合称为 `FactExtractionProjection`，读取方法名体现 projection；Messaging 查询参数使用 `message_id`。
- scheduled ToolRun 的本地身份变量明确写为 `tool_run_id`，在观察、持久化与定时触发上下文中标明被追踪的 ToolRun。
- Common/LLM 配置 API 使用 `model_config` / `model_config_id`，并将 `RoutedModel` 的内部身份字段命名为 `model_config_id`。
- 保留 `ModelConfig.id` 的既有 TOML/Settings key、provider model name 与 TTS provider payload 字段；`RoutedModel` 序列化仍使用既有 wire key `id`。

## 影响

这些修改明确 Rust 调用图、参数和内部模型身份，不改变运行时策略、SQLite schema、事件 payload 或用户配置 key。现有测试覆盖 `RoutedModel` serde round-trip。

## 验证

- Memory、Agent、Messaging、Common focused tests/checks
- `cargo check --locked -p haven-app-binary -p haven-llm`
- `cargo fmt --all -- --check`、`git diff --check`

## 回滚

如回滚内部 API 名称，仍不得把配置记录 ID、provider model name 与泛化 `model` 混为一谈；Serde/TOML `id` 和 TTS payload key 继续保持原样。
