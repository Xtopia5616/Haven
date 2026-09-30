# 0415：事实抽取结果与完成状态原子提交

> 状态：已采纳
> 日期：2026-09-30

## 背景

普通事实抽取先提交 facts，再单独写 `fact_extraction.{session_id}`。游标写入失败会使同一 transcript window 重试，并再次提高已有事实的 `mention_count` 和 confidence。

摘要抽取用单个 `fact_extraction_episode.{session_id}` 保存最近 episode。durable outbox 按无序集合恢复；较旧 episode 在较新 episode 后重试时不再等于该游标，可能再次写入事实并回退游标。摘要 marker 的 ack 本身也可能失败。

## 决定

- 普通抽取在一笔 SQLite 事务中 upsert facts 并写入 message cursor。包括有效空结果在内，cursor 写入失败时事实变更一并回滚。
- 摘要抽取使用 `fact_extraction_episode_done.{session_id}.{episode_id}` 逐 episode 记录完成状态，值为 `session_id`。facts 与 marker 在一笔 SQLite 事务中提交；marker 已存在时跳过事实写入。
- 将 fact graph 的事务钩子限制为同步 SQLite 连接操作；Agent 仍负责抽取、清洗与置信度策略，MemoryFactStore 负责事务和持久化边界。
- 保留旧的最近 episode KV key 仅用于既有数据清理，不再读取或写入。新增状态使用既有 `kv_store`，不改 schema。

## 替代方案

可以为所有抽取事实设计通用幂等键，或引入独立的完成表。事实提交和当前 cursor/marker 已能在同一 SQLite 事务内表达相同的重试边界，因此不增加额外事实索引或 schema。

## 影响

- 普通 cursor 和 facts 不再有分离提交窗口。
- 每个摘要 episode 独立完成；无序重放与 marker ack 重试不会重复强化已提交事实。
- 完成 marker 随 session 删除、历史清理和孤儿状态维护一同回收。
- 不需要数据库重置或迁移。旧 episode cursor 不参与新版本判定。

## 验证与回滚

- 回归用例注入普通 cursor 写入失败、摘要完成 marker 写入失败及摘要 outbox ack 失败，验证事实回滚或重试幂等；另覆盖较旧 episode 在较新 episode 之后重试。
- 已运行 `cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`，以及 UI 的 `check`、`test:run`、`build`。
- 回滚代码即可；通用 KV 中残留的逐 episode marker 不影响旧版本，并可随 session 删除或孤儿清理回收。
