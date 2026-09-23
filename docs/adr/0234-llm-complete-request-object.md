# ADR 0234：LLM 完整请求对象与唯一 complete 入口

- 状态：已采纳（2026-09-24）
- 范围：`haven-llm` 的非流式 chat/tool 请求，以及仓库内完整请求调用点
- 关联：[ADR 0221](0221-chat-request-policy-snapshot.md)、[ADR 0228](0228-llm-chat-alias-removal.md)

## 背景

Router 已将普通 chat 和工具 chat 收敛到同一 permit、retry、timeout、health 与 usage 执行边界，但仍通过 `chat_request`、output-cap 变体和 `chat_with_tools` 公开多组参数转发入口。请求属性分散在参数列表中，调用者需要根据是否携带 tools 选择不同方法。此前 ADR 0228 删除旧 chat 别名后，`chat_request` 一度成为主入口；本决定继续收口为完整请求 DTO。

## 决定

1. 在 `haven-llm` 定义并导出 `CompleteRequest`，集中承载 `RequestKind`、canonical messages、tools 和可选 `max_output_tokens`。
2. `LlmRouter::complete(CompleteRequest)` 是唯一非流式 chat/tool Router 请求入口。删除 `chat_request`、`chat_request_with_output_cap`、`chat_with_tools` 与 `chat_with_tools_output_cap`；不保留兼容别名。
3. `tools` 为空时继续调用 provider 的普通 chat output-cap 方法；`tools` 非空时继续调用 tools output-cap 方法。DTO 不改变调用策略或 provider wire 行为。
4. `chat_with_prompt` 和 `chat_messages_cancellable` 保留为有语义 helper，并直接构造 `CompleteRequest`。此前保留的 `chat_with_prompt_output_cap` 后因无仓库调用者而由 ADR 0250 删除；输出上限仍由 `CompleteRequest::max_output_tokens` 表达。取消仍使用 biased select，使取消在同时就绪时优先，并在取消后丢弃正在执行的 provider future。
5. 每个完整请求仍先通过已有路由、capability、permit 和 circuit gate，再于 permit 内读取一次 retry/total-timeout 策略快照；输出 cap 原样传递，retry、timeout、health 和 usage 仍沿用既有管线。
6. 迁移 vision 与 file-summary 调用点及所有仓库内 Router 完整请求调用。`LlmClient` 和 provider adapter 的 chat 方法仅作为下层 provider 执行契约保留，不属于 Router 兼容入口。
7. 本切片不拆 `Capability` / `CallPurpose` / `RequestPolicy`，不重写 request pipeline 的策略类型，不改 stream、embedding、health API、provider adapter、wire fixture 或 usage 持久化。

## 替代方案

- 保留旧 Router 方法作为包装别名：继续暴露重复的入口并延长旧 API 生命周期，拒绝。
- 同时对象化 stream/embed/health 或拆分 executor：超出本切片；这些入口各自有不同的流式生命周期或结果契约，放在后续独立切片。
- 现在重构 Capability / CallPurpose 或 provider adapter：会把路由分类与 wire mapping 混入请求 DTO 收口，拒绝。

## 影响与验证

`LlmRouter` 的内部 Rust API 发生破坏性收缩，仓库内调用点同步迁移。无数据库、配置、IPC 或用户数据格式变化。测试覆盖普通请求、空 tools 与非空 tools 分支、output cap 与 usage 透传、取消优先级，以及缺少所需 capability 时拒绝路由；provider adapter 与 wire fixture 不变。

验收命令：

```sh
cargo fmt --all
cargo test --locked -p haven-llm
cargo test --locked -p haven-tools
cargo test --locked -p haven-agent
cargo clippy -p haven-llm --locked -- -D warnings
```

## 回滚

以单个 commit 回滚本切片即可恢复此前 Router 方法与调用点；无数据重置要求。若继续向外收窄 Router API，应迁移所有调用方，不应重新引入本 ADR 删除的兼容别名。
