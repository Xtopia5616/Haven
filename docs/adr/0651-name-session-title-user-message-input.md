# ADR 0651：按 user-only 数据命名 Agent Session 标题输入

## 状态

已采纳并实施。

## 背景

Memory `SessionStore::title_generation_context` 从最近 eligible session messages 中只筛选 user role，并按既有 chronological order 返回 `SessionTitleGenerationContext.user_messages`。Agent `TitleGenerator::generate` 却把该 slice 称为 `conversation`，拼接值称为 `conv_text`，测试名也称为空 conversation；这些名称会让调用者误以为输入包含完整 session transcript。

## 决定

1. `TitleGenerator::generate` 参数统一命名为 `user_messages`，拼接文本叫 `user_message_text`。
2. 相关文档、注释和空输入测试名说明此入口消费 user-only 消息。
3. Memory 查询、消息筛选/顺序和标题请求内容保持不变。

## 替代方案

- 继续称作 conversation：拒绝。Memory read contract 明确不返回 assistant 或工具消息。
- 把 title context 改成完整 transcript：拒绝。标题来源由用户输入决定，拓宽数据不符合现有查询和调用语义。

## 影响与验证

只重命名 Agent 方法参数、局部值和测试说明，不改变 Memory API、LLM 请求或 title 持久化；无需重置。验证：workspace fmt、Agent crate locked check、严格 Clippy 与测试，以及 ADR index。

## 回滚

恢复旧 Rust 参数/局部名并同步撤回命名规范、路线图与 ADR 索引；无持久状态迁移。
