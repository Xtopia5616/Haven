# ADR 0654：统一 Memory fact candidate 持久化策略入口

## 背景

MemoryWorker 的生产路径把模型结果 `LlmFact` 与 transcript 来源引用投影为 `MemoryFactWrite`；测试路径另有 `FactDraft`，用 7 项 tuple 按位置表达同一候选，再由 `persist_fact_batch` 重复实现清洗、敏感值过滤、数值约束、标签归一和写入结构构造。策略重复可能让测试与生产行为漂移，tuple 字段也只能依赖注释中的顺序识别。

## 决定

- 删除 test-only `FactDraft` tuple alias。
- 引入私有具名 `MemoryFactCandidate`，表达已解析 transcript provenance、尚待共享策略清洗的事实字段。
- 生产解析和测试写入都调用 `prepare_fact_candidates`，统一构造 `MemoryFactWrite`；生产路径消费 `LlmFact` 所有权，避免因候选投影额外复制字段。
- 保留测试专用的存储入口，它只负责把共享转换结果交给 MemoryFactStore；它不再实现另一份清洗策略。
- 不改变抽取提示、过滤/阈值语义、provenance、事务提交或数据库形状；没有兼容 alias。

## 验证

- `cargo fmt --all -- --check`
- `cargo test --locked -p haven-agent`
- `cargo clippy --workspace --locked -- -D warnings`

## 回滚与重置

回滚时恢复测试 tuple 和独立转换实现。本次只影响 Agent 内部代码，不改变外部 API、配置、IPC 或数据库，不需要数据重置。
