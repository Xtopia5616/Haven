# ADR 0691：区分 Common 与 LLM 工具定义

## 状态

已采纳并实施。

## 背景

`haven_common::tools::ToolDef` 是 Tools、Agent 与 App 共用的 provider-neutral 模型调用定义。
`haven_llm::types::ToolDefinition` 则是 LLM request 边界使用的 `{type, function}` 中间形状；
它会规范化参数 schema，再由各 provider adapter 转成自己的 wire shape。Agent 也消费该
LLM 类型计算请求 token 预算。两个公开类型职责不同，但同名近似会让跨 crate import 难以
辨认来源与阶段。

## 决定

- 将 LLM 类型改名为 `LlmToolDefinition`，并更新所有 workspace 消费者，不保留 `ToolDefinition` alias。
- 保持 `ToolDef → LlmToolDefinition → provider adapter wire` 的转换层次；`ToolDef` 仍是共享定义，LLM 类型不扩展成新的公共 catalog。
- Serde 字段、schema sanitization、provider payload 和 Agent token estimate 行为保持不变。

## 替代方案

- 合并两个类型：拒绝。`ToolDef` 携带跨 provider/runtime 的语义，`LlmToolDefinition` 是 LLM adapter 的请求形状，字段和生命周期不同。
- 保留旧名并加说明：拒绝。该类型跨 crate 使用且已可由 owner 名明确区分；仓库不要求内部 Rust API 向下兼容。

## 影响与验证

- 这是 workspace 内的 Rust API 重命名，涉及 `haven-llm`、Agent 和 provider adapters；不改变 IPC、持久化、配置或 provider wire contract。
- 验证：`cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、ADR 索引检查和 `git diff --check`。

## 回滚

恢复 `ToolDefinition` 名称并同步 workspace 调用点即可；无数据或外部协议迁移。
