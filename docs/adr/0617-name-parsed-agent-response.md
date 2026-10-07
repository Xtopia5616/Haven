# ADR 0617：命名 ParsedAgentResponse

## 状态

已采纳并实施。

## 背景

`ReActEngine::parse_default_model_response` 将 provider 的 `LlmResponse` 转换为 Agent 当前 step 使用的 thought 文本与 `ToolCall` 列表。主执行路径、响应重试路径和测试都按 tuple 位置读取这两个稳定结果；调用方需要从绑定顺序推断各值的含义。

## 决定

1. 用 `ParsedAgentResponse { thought, tool_calls }` 表达 ReAct 解析阶段的结果，并从 `haven_agent` crate root 导出。
2. 执行路径、重试路径和测试按具名字段读取结果。
3. 保持 thought 过滤、隐式 final-answer 生成、tool-call id 兜底及执行策略不变。

## 替代方案

- 保留 tuple：拒绝，两个值都有稳定且不同的领域语义，多处调用方都需要依赖位置约定。
- 复用 `LlmResponse`：拒绝，它表示 provider 的原始响应，不包含 Agent 归一后的 thought 与 final-answer tool-call 语义。
- 只在调用方各自定义局部结构：拒绝，会重复表达由 ReAct parser 单一产生的同一结果。

## 影响与验证

- 这是 `haven-agent` 的 Rust source API 返回类型调整；workspace 调用方已迁移，provider wire、持久化和 transcript 语义不变。
- 命名审计 §5.7 保持 Active；其他 crate 函数、组件和跨层 contract 仍待完整盘点。
- 验证：workspace fmt、check、strict Clippy、tests、ADR 索引及 staged diff 检查。

## 回滚

恢复 tuple 返回值并同步 ReAct 执行、retry、测试和 crate root export；无需数据或 wire 迁移。
