# ADR 0606：命名 LLM resolved model client

## 状态

已采纳并实施。

## 背景

`ModelDirectory::resolve_client` 为可执行请求解析已校验的 primary model id 和对应 `Arc<dyn LlmClient>`，原返回 `(String, Arc<dyn LlmClient>)`。`LlmRouter` 使用 model ID 关联并发 permit、rate-limit cooldown 与 health/circuit 状态，使用 client 发送请求；manual retry 只需要该 model 的 ID 并丢弃 client。tuple 没表达身份和执行 adapter 的不同职责。

## 决定

1. 解析结果改为 crate-private `ResolvedModelClient { model_id, client }`。
2. Router 请求执行、流式调用与 manual retry 按字段分别使用 model identity 和 client adapter。
3. `select_client` 的非执行 helper 继续保留原 default-adapter fallback，不改为 `resolve_client` 的别名或共享结果。

## 替代方案

- 保留 tuple，只给调用点绑定具名局部变量：拒绝，ModelDirectory 的实际职责契约仍靠位置表达。
- 合并 `select_client` 与 `resolve_client`：拒绝，前者用于非执行选择并保留默认 adapter fallback，后者要求配置的 primary route 与 client 均有效后才能执行。

## 影响与验证

- 仅改变 `haven-llm` crate 内部类型，不改变 request policy 解析、primary route 选择、默认 fallback、健康状态 key、限流或 provider 请求行为。
- 无 IPC、持久化、配置或 provider wire shape 变化；命名路线图 §5.7 继续保持 Active。
- 验证：`cargo fmt --all -- --check`、`cargo check --locked -p haven-llm`、`cargo clippy --locked -p haven-llm -- -D warnings`、`cargo test --locked -p haven-llm`、ADR 索引及 staged diff 检查。

## 回滚

恢复 `resolve_client` 返回 `(String, Arc<dyn LlmClient>)`，并还原三个 Router 调用点与目录测试的 tuple 解构；无需配置或数据库迁移。
