# ADR 0875：区分通用与 Chat 手动重试准备

## 状态

Accepted — 2026-10-10

## 背景

`LlmRouter::prepare_manual_retry(RequestKind)` 对调用方指定的请求路线重置对应 endpoint circuit 的连续失败 gate。`ReactEngine::prepare_manual_retry()` 是一个窄 session 边界，只为显式 Continue 固定传入 `RequestKind::Chat`。两层同名掩盖了请求范围差异，也容易让 Agent 调用者误以为上层可为其它请求类型准备重试。

## 决定

- 将 Agent wrapper 改名为 `prepare_manual_chat_retry()`，保留 `LlmRouter::prepare_manual_retry(RequestKind)` 的通用名称。
- 该 wrapper 继续只绑定 Chat；session Continue 的顺序与行为不变。
- Endpoint circuit 的状态重置、cooldown 和计数策略仍由 LLM router/circuit breaker 拥有，Agent 不复制这些规则。

## 影响与兼容性

本次只重命名 crate-private Rust 方法，不改变 LLM 请求、session 状态、IPC、配置或持久化行为。无需重置；不保留旧方法名 alias。

## 验证

通过：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、
`cargo clippy --workspace --locked -- -D warnings`，以及新 ADR 文件的 Prettier 检查。
测试套件未运行。

## 回滚

若 Agent 后续支持多种手动重试路线，应增加一个带显式请求种类的 Agent API，而不是将 Chat 专用 wrapper 扩成隐式通用入口。
