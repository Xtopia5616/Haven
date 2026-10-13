# ADR 1153：为长期事实列表建立有界 keyset 分页

## 状态

已采纳并实施；2026-10-13 Rust、UI 与 IPC 自动门禁通过。

## 背景

`list_facts` 原先将持久化事实全部读入内存并返回给 Memory 页面。提取提示词的 64 条上限不约束这个 UI 列表。全量返回会随事实库增长增加 SQLite、IPC 和 DOM 的工作量。

有效置信度会随时间衰减，因此它不能作为跨请求稳定的游标顺序。敏感事实还必须在页面配额计算前过滤，避免隐藏行占满一页。

## 决定

- Memory 的事实列表一次最多返回 100 条，默认 50 条。来源筛选在数据库查询内应用。
- 使用 `(created_at DESC, id DESC)` 作为稳定顺序，游标携带上一页末行的 RFC3339 `created_at` 和规范 `fact-*` ID。每次请求读取当前数据库状态，不承诺跨页快照；新事实位于其创建时间对应位置，继续旧游标不会回头读取更靠前的新行。
- 无来源筛选和按来源筛选分别使用 `(created_at DESC, id DESC)` 与 `(source, created_at DESC, id DESC)` 索引；SQL 分支保持来源等值条件显式，以便 SQLite 用匹配索引完成 keyset 遍历和排序。
- SQL 使用由共享敏感规则生成的可见性谓词，在 `LIMIT` 前排除敏感 subject、predicate 与 object；加载后仍执行 Memory 可见性检查，敏感来源摘录按现有规则脱敏。
- UI 首次只加载一页，用户可继续加载下一页。来源变化和添加事实后从第一页重新开始；请求序号丢弃旧筛选条件的晚到响应。
- 此变更不修改已存事实或数据库行契约；新增索引由幂等 schema 初始化创建，不需要重置数据。回滚只需恢复旧 command 与 UI 契约并可移除索引定义。

## 验证

- Memory 边界测试覆盖大于最大页大小的数据集、敏感行过滤、来源筛选、稳定游标遍历、无重复和无效游标拒绝；`EXPLAIN QUERY PLAN` 检查两种查询都使用匹配索引且不执行临时排序。
- 通过 `cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo fmt --all -- --check`、UI check、UI 全量测试（153 个文件 / 1283 项）和 production build。
- IPC command contract（80 handlers）与 event contract（36 channels）检查通过；事实分页的 `EXPLAIN QUERY PLAN` 回归确认两种查询都使用匹配索引且不创建临时排序。
