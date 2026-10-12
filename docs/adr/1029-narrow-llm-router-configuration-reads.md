# 1029：收窄 LLM Router 配置读取

## 状态

已采纳并实施（2026-10-12）。

## 背景

`LlmRouter::config()` 将完整的 `RouterConfig` 读锁暴露给跨 crate 调用方。该配置含 API key、base URL、代理与认证设置；Agent、Tools 和 Admin 实际只需少量路由能力或诊断元数据。消费者读取整份配置也把 provider 协议判断与 endpoint 细节扩散到了 LLM crate 之外。

## 决定

- 删除完整配置 getter。路由配置始终由 `LlmRouter` 独占。
- Agent 使用 Router 已有的 `context_window_for_request` 读取解析后的上下文窗口；embedding index 使用 `embedding_route_identity` 获取 provider model ID 和不含 endpoint 的 opaque vector-space ID。
- Tools 使用 `transcription_available` 与 `provider_web_search_available` 查询 LLM-owned 路由能力；MCP 搜索和专用 STT 可用性仍由 Tools 合并。
- Admin 使用 `request_route_diagnostics` 读取固定安全 DTO，包含 route issue、model config ID、provider 和 provider model ID；不含 API key、base URL、proxy 或认证字段。原有 issue 分类与优先级保持不变。
- 删除跨 crate 已无消费者的 `api_style_for`、`resolve_web_search_mode`、`is_known_api_style` 根导出，并将 `api_style_for` 收为 LLM crate 内部函数。
- embedding vector-space ID 保持既有 `embedding-v2` SHA-256 输入顺序、规范化字节和输出格式；固定值测试锁定存量向量索引身份。

## 影响

跨 crate 调用方不能读取 Router 的完整配置，只能请求其职责所需的解析值。Diagnostics 与 provider capability 行为保持现有结果；Tauri wire shape、SQLite schema、配置文件和 embedding 索引 key 均不变，无需重置数据。

## 验证

- `cargo fmt --all -- --check`
- `cargo test --workspace --locked`
- `cargo test --locked -p haven-llm --lib`
- `cargo check --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `scripts/check-ipc-contracts.ps1` 与 `scripts/check-ipc-events.ps1`
- `scripts/check-adr-index.ps1` 与 `git diff --check`

## 回滚

若回滚消费者改动，仍应保留窄接口并为缺少的能力增加明确方法；不要恢复返回完整 `RouterConfig` 的 getter。embedding identity 算法只有在另行迁移或重建持久向量索引后才可改变。
