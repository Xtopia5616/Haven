# ADR 0054：Agent 事实推理边界

## 背景

`InferenceEngine` 同时负责事实抽取调度、LLM 调用、数据库写入，以及抽取窗口、
transcript 构造和矛盾/谓词提案安全门禁。后几项是可独立验证的事实推理策略，
继续内嵌在 engine 中会使 Agent 热点文件同时承载编排与纯策略逻辑。

## 决定

- 新增内部 `crates/agent/src/fact_inference.rs`，集中承载增量抽取窗口、assistant/
  tool 上下文裁剪、低信任过滤、来源消息解析、编号 transcript 和提案门禁。
- `inference.rs` 继续负责节流、outbox、LLM 调用、事实写入、embedding 索引和
  维护调度；通过内部函数调用事实推理模块，不改变持久化或 Agent 事件契约。
- 保持 X12 投影读取、用户来源优先、敏感字段过滤、矛盾 keeper 保护、0.85 置信度
  门槛和 canonical predicate 规则等既有语义。

## 替代方案

- 继续把抽取窗口和门禁留在 `inference.rs`：会延续事实策略与后台编排混合，拒绝。
- 把数据库写入或 LLM 调度一起下沉：边界过大，会改变 `InferenceEngine` 的生命周期
  与节流责任，拒绝。

## 影响

这是 Agent 内部模块重组；`inference.rs` 的事实推理策略减少约 350 行，LLM、数据库、
embedding、IPC 和事件契约不变。

## 验证

```text
cargo fmt --all -- --check
cargo check --workspace --locked
cargo test --locked -p haven-agent -- --test-threads=1
```

重点回归增量窗口、低信任 kickoff 过滤、tool observation 合成、来源解析、transcript
截断和事实提案门禁测试。

## 回滚

将 `fact_inference.rs` 中的内部策略函数重新并回 `inference.rs`，恢复原调用位置；
不需要数据库、配置或用户数据迁移。
