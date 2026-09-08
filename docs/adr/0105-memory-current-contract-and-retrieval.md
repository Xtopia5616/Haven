# ADR 0105：记忆系统当前契约与严格召回

- 状态：Accepted
- 日期：2026-09-08
- 范围：`haven-memory` 的 SQLite schema、事实图谱、FTS5、embedding 与混合召回

## 背景

记忆层长期保留了历史数据库迁移、旧 embedding 域名、未知 polymorphic domain、
多种 FTS 建表形状以及 FTS 失败后的 LIKE 退化。这些分支让数据库看似可打开，
却可能在缺少索引、所有者或一致向量空间时静默改变召回结果。测试阶段没有必要
为旧内部形状维持这些兼容入口。

## 决策

1. 当前数据库契约固定为 schema v16。`init_schema` 只幂等创建当前 schema；旧版本、
   未版本化但已有用户表的数据库直接拒绝打开，用户按发布说明重置数据库。删除
   `migrations.rs`，不再在运行时执行历史 data migration。
2. `memory_fts` 使用当前统一 FTS5 trigram 表，FTS5 是必需能力。fact/episode 搜索
   不再吞掉 prepare、MATCH 或行映射错误并退化为长文本 LIKE；只有 trigram 无法索引
   的一、两个字符词使用有界 LIKE/近期 episode 扫描补充。
3. embedding 的 `entity_type` 只允许 `fact` 和 `episode`，写入必须拥有真实 owner、
   非空文本和有限浮点向量；同一 model 在所有域中只能使用一个维度。LSH 派生表与
   主 embedding 表共享同样的封闭域约束。
4. 召回查询只接受规范 kind 和有界 limit，混合 keyword/vector 结果使用 Reciprocal
   Rank Fusion（RRF）合并，按实体 ID 去重，不再依赖候选 text 相等。
5. fact provenance 摘要在写入前限制为 120 字符，并将敏感内容替换为 `[redacted]`；
   图谱写入同步校验 subject/predicate/object、source、confidence 与 durability。

## 后果

- 发布该版本时必须清除 `haven.db`、`haven.db-wal`、`haven.db-shm`；数据库中的会话、
  记忆、任务、快照和用量不会自动迁移。配置可以单独保留。
- FTS/embedding 故障变成可诊断错误，不会悄悄产生低质量召回；短词仍有明确、受限的
  精确补充路径。
- 标准 FTS5 表会保存索引文本，换取实现更简单、行为一致和 SQLite 版本依赖更少。
- 未来 schema 或召回契约变化应增加新的明确 reset/release 说明，而不是重新引入
  未知域、旧别名或静默 fallback。

## 验证

`haven-memory` 单元测试覆盖当前 schema 拒绝旧/新版本、域和值约束、FTS trigger、
embedding owner/维度约束、短词补充和 RRF 混合排序。
