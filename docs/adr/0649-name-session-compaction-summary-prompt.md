# ADR 0649：按 Session compaction 用途命名共享 prompt 常量

## 状态

已采纳并实施。

## 背景

`haven_common::prompts::CONVERSATION_SUMMARY_PROMPT` 仅由 Agent compactor 使用，为 Session transcript 压缩生成续接摘要提供 instruction prefix。常量名把其领域叫作 conversation，未标出真正 owner/lifecycle——Session compaction；提示正文使用 conversation 描述待总结的自然语言内容是正确的模型语义。

## 决定

将跨 crate 常量改名为 `SESSION_COMPACTION_SUMMARY_PROMPT`，并同步唯一消费者与文档。常量内容逐字保持不变。

## 替代方案

- 保留 `CONVERSATION_SUMMARY_PROMPT`：拒绝。名称未标出 Agent compaction 所属的 Session 领域与执行阶段。
- 把 prompt 正文中的 conversation 一律替换为 session：拒绝。正文描述输入给模型的自然语言会话内容，与 Haven 持久实体名是不同语义层。

## 影响与验证

变更 Common→Agent 的 Rust 符号名，不影响摘要文字、预算、LLM 请求、持久数据或 wire contract；不需要数据重置。按跨 crate 检查运行 Rust workspace fmt、locked check、严格 Clippy 和 workspace tests，并检查 ADR 索引。

## 回滚

恢复旧常量与 import，并同步撤回命名规范、路线图和 ADR 索引；没有状态迁移。
