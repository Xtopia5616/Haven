# ADR 0029：Agent 事实抽取边界

## 背景

`InferenceEngine` 同时承担事实抽取调度、上下文窗口、LLM 响应解析、持久化和
矛盾处理。事实抽取的模型 wire shape、字符串强制转换、标签/谓词规范化、字段
注入防护和 JSON array 提取是可独立审查的稳定规则，却与编排状态机混在
`inference.rs` 中。

## 决定

- 新增 Agent 内部模块 `fact_extraction.rs`，集中拥有 `LlmFact`、`FactDraft`、
  LLM 字段 coercion、标签白名单、谓词规范化、事实字段清洗和 JSON array 提取。
- `InferenceEngine` 继续拥有 outbox、节流、上下文窗口、LLM 调用、事实候选
  过滤和持久化；只通过内部函数使用抽取边界。
- 事实谓词规范化继续委托 `haven-memory` 的统一实现；事实字段清洗继续复用
  `haven-common` 的 prompt sanitizer；保持字段默认值、字符串 coercion、标签
  白名单和 JSON 解析行为不变。
- `fact_extraction.rs` 只处理模型输入/输出数据，不引入新的持久化表、IPC
  契约、ID 空间或并发策略。

## 替代方案

- 继续将抽取 DTO 与解析器放在 `inference.rs`：会让编排修改扩大回归面，拒绝。
- 将抽取规则下沉到 Memory：会让持久化层拥有 LLM wire 语义，违反层边界，拒绝。
- 将 `LlmFact` 提升为跨 crate 公共类型：当前仅 Agent 消费，扩大 API 兼容面，
  暂不采用。

## 影响

这是 `haven-agent` 内部模块重组。事实抽取提示、解析、清洗、谓词规范化、持久化
和 kv cursor 语义不变，不需要数据库、配置或缓存重置。

## 验证

```text
cargo fmt --all -- --check
cargo check --locked -p haven-agent
cargo test --locked -p haven-agent --lib -- --test-threads=1
cargo clippy --locked -p haven-agent --lib -- -D warnings
```

重点回归非字符串字段 coercion、durability/default subject、标签白名单、谓词
alias、prompt sanitizer 和 JSON array 提取。

## 回滚与重置

代码回滚时删除 `fact_extraction.rs` 和模块登记，把其类型与函数恢复到
`inference.rs`；本次不改变 schema、序列化、配置或持久化数据，不需要用户重置。
