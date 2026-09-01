# ADR 0065：上下文、工具与记忆边界的失败安全收紧

## 背景

ReAct 的三条高频路径仍存在同一类风险：上下文组装会把数据库故障误判为空，
历史预算可能从头部截断而丢失最新输入；工具的后台/定时副作用与 action-step
投影不是同一提交边界；记忆检索存在查询范围、敏感 provenance、事实替换原子性
和向量空间身份不完整的问题。它们在正常路径上不明显，却会在重启、并发、取消、
配置切换或数据库短暂失败时产生静默丢数据或错误重试。

## 决定

- 上下文按最近完整条目倒序装入预算；只有单条最新条目过大时才截断。会话描述、
  历史和工具/技能/MCP 元数据均按“引用数据”处理并做边界清洗。记忆读取失败只
  降级为本轮空 MEMORY，不缓存这个空结果；下一轮继续重试。
- 所有 ReAct 恢复和记忆查询的 SQLite 读取都通过 blocking 边界执行，并传播关键
  错误。事实提取在游标、步骤或节流状态读取/写入失败时不继续调用模型，也不推进
  游标。
- 后台 action 在进程启动前先持久化 owner 记录；action 锁不跨数据库或通知 await。
  工具副作用已发生但 action-step 投影失败时返回“结果未知”，禁止把它伪装成普通
  可安全重试的失败。定时 action 的 cap、set、fire、cancel 由独立 mutation gate
  串行化。
- 记忆事实写入由同一 Database 句柄串行，用户单值事实替换使用 SQLite 事务；检索
  在 SQL LIMIT 前应用 subject scope，并隐藏敏感 source snippet。向量索引的持久化
  身份由 provider、有效 wire style、endpoint 和 model 的 SHA-256 派生，维度变化或
  旧身份存在时清空并重建派生索引。
- 本轮不改变数据库 schema；向量身份升级会自然触发已有向量派生表的重建。取消后台
  进程的最终 OS 进程树回收仍由后续专门生命周期改造处理，不在本 ADR 中伪造同步完成。

## 替代方案

- 继续以空集合吞掉读取错误：短，但会把故障缓存成“没有记忆”，拒绝。
- 仅以 model name 作为向量身份：实现简单，但同名模型经不同 gateway 或 endpoint
  可能不在同一向量空间，拒绝。
- action 先启动后补写数据库步骤：正常吞吐略高，但崩溃窗口会留下无 owner 的副作用，
  拒绝。
- 把投影失败继续映射为普通 Failed：调用方会自动重试不可幂等工具，拒绝。

## 影响与验证

这是 Agent、Tools、Memory 和 prompt 边界的内部重构，不保留旧向量身份；首次运行
新版本可能重建 embedding/LSH 派生索引。用户记忆、会话 transcript 和工具 wire
结构不需要迁移。验证覆盖：最新上下文预算、提示注入清洗、数据库错误不缓存、事实
游标失败安全、单值事实事务、subject/sensitive 过滤、同名模型跨 endpoint 分区、
后台 action durability、定时 mutation 顺序、未知副作用结果。

```text
cargo fmt --all -- --check
cargo check --workspace --locked
cargo clippy --workspace --locked -- -D warnings
cargo test --workspace --locked
cd ui && corepack pnpm run check && corepack pnpm run test:run
```

## 回滚 / 重置

回退本 ADR 对应提交即可恢复代码路径；不需要数据库迁移。若回退后出现旧向量身份
与当前派生索引不一致，删除并重建 `memory_embeddings` / `memory_embedding_lsh`
派生数据即可，事实、episode 和 transcript 不受影响。
