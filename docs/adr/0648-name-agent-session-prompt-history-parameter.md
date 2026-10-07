# ADR 0648：统一 Agent system prompt 历史上下文参数名

## 状态

已采纳并实施。

## 背景

Agent 的最近 session 消息加载器、输入 DTO、run 参数与窗口 limit 已按 ADR 0560 使用 `session_prompt_history` / `SessionPromptMessage`。但 `SystemPromptBuilder::build`、`build_for_session`、无 Memory 变体和内部预算渲染 helper 仍把同一批输入称为 `conversation_history` 或泛称 `history`，使明确的 prompt-history 领域词汇在最后一个构造边界丢失。

## 决定

1. `SystemPromptBuilder` 所有接收该数据的入口参数统一命名为 `session_prompt_history`。
2. `render_recent_context_with_budget` 的对应输入也统一为 `session_prompt_history`，不引入另一层泛称。
3. 提示正文描述会话内容时仍可用自然语言 “conversation”；只统一 Rust 参数/局部 API 的用途名称。

## 替代方案

- 保留 `conversation_history`：拒绝。它未表达这是 Agent 加载后专供首次 system prompt 的附加上下文，且与其余调用链命名不一致。
- 全文机械替换所有 conversation 用词：拒绝。自然语言 prompt 内容与外部消息线程等其他领域可以有独立语义，不应改写成代码领域名。

## 影响与验证

仅重命名 Agent 内部 Rust 输入参数，不改变 prompt 文本、消息顺序、预算、过滤或持久/wire 契约；无需数据重置。验证：Rust fmt、Agent crate locked check、严格 Clippy 与 Agent crate tests。

## 回滚

恢复旧 Rust 参数名并同步撤回命名规范、路线图和 ADR 索引；不涉及状态或数据回滚。
