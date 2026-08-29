# ADR 0021：Agent 记忆嵌入编排边界

## 背景

`InferenceEngine` 既负责事实抽取与维护策略，又负责 embedding endpoint 调用、
待索引批次收集、模型切换失效、向量召回和 LSH 重建。这样 provider 网络调用与
事实推理规则处在同一个热点实现中，修改索引生命周期容易误触抽取或维护行为。

## 决定

- 新增 Agent 内部组件 `MemoryEmbeddingIndex`，唯一负责 embedding endpoint 的
  配置判定、有限批次 catch-up、向量召回、模型变更清理和 LSH 派生表重建。
- `InferenceEngine` 只保留事实抽取、事实维护编排和 keyword fallback；热路径与
  scheduler 通过 `MemoryEmbeddingIndex` 调用嵌入能力。
- SQLite 仍由 `haven-memory::Database` 负责：候选 ID、源文本、embedding 行、
  缓存失效和 LSH 数据的持久化语义不改变；Agent 组件不得直接持有连接跨越网络
  await。
- 待索引工作继续使用 Memory repository 的 fact/episode backlog 上限，并将每次
  provider 请求限制在最多 10 个文本；embedding 未配置、调用失败或向量为空时，
  召回回到既有 keyword/FTS 路径。

## 替代方案

- 继续把 embedding 操作留在 `InferenceEngine`：改动较小，但 provider 生命周期与
  事实推理继续耦合，拒绝。
- 把 embedding endpoint 调用下沉到 `haven-memory`：会让持久化 crate 依赖 LLM
  provider，违反 crate 单向依赖，拒绝。
- 同时拆出完整的 Memory maintenance service：会把事实清理、LLM 仲裁与本切片的
  索引边界混在一起，扩大行为变化范围，留待后续独立切片。

## 影响

这是 Agent 内部结构重组，不改变 `Database` API、数据库 schema、embedding 表、
模型配置、召回 wire DTO 或 ID/缓存契约，不需要用户重置。embedding 的网络调用
仍经 `LlmRouter`，并继续受 router 的超时、重试和 provider 能力降级约束。

## 验证

```text
cargo fmt --all -- --check
cargo check --locked -p haven-agent
cargo test --locked -p haven-agent --lib -- --test-threads=1
cargo clippy --locked -p haven-agent --lib -- -D warnings
```

新增边界测试覆盖 provider-safe 批次上限和跨 fact/episode 的有界候选收集；既有
Agent 抽取、维护和 Memory embedding 存储/索引测试继续覆盖行为契约。

## 回滚与重置

代码回滚时删除 `memory_index.rs`，将其方法恢复到 `inference.rs`，并恢复
`InferenceEngine` 的字段与调用点。由于本次不修改 schema、数据或配置格式，不需要
数据库、配置或缓存重置。
