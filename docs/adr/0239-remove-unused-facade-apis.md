# ADR 0239：删除已无调用者的 facade API

- 状态：已采纳（2026-09-24）
- 范围：`haven-llm`、`haven-tools`、`haven-common` 的仓库内兼容 facade
- 关联：[ADR 0228](0228-llm-chat-alias-removal.md)、[ADR 0234](0234-llm-complete-request-object.md)

## 背景

仓库已完成 Router 请求入口收口，但仍保留三个没有仓库调用者的兼容面：LLM 根路径 `with_retry` re-export、
`ToolsManager::set_router` 和 `CapabilityScope` 的借用转换。它们增加可见 API，却没有提供运行时能力。

## 决定

1. 删除上述三个无调用者 API。
2. `session.rs` 的两个调用点显式 clone `CapabilityScope`，不再依赖隐式转换。
3. 保留 `crate::client::with_retry`、`set_router_and_media_clients` 以及仍有调用的 chat/cancellable helper。

## 影响与验证

只减少仓库内 API 表面积，不改变 provider、tool execution、授权或配置运行时行为。workspace check、测试和严格 Clippy 全部通过。

## 回滚

恢复三个 API 与两个显式 clone 调用点；不涉及持久数据或 IPC。
