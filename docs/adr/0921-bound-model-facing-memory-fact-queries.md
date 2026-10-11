# 0921：由 Memory 限制模型工具的事实查询结果

## 状态

已接受并实现（2026-10-11）。

## 背景

MemoryTool 的 search/list 先从 `MemoryFactStore` 取得可见事实，再由 Tools 按请求 limit 截断。全文查询在某些 FTS 路径下没有数据库候选上限，因此未限页的事实集合会跨 crate 返回。subject list 和跨 subject list 也通过不同的无界方法暴露给 Tools；跨 subject 方法名中的 `recent` 不准确，因为真实顺序是 effective confidence、last-seen、created-at。

Tools 还在调用 `search_visible_facts` 前调用 `normalize_memory_query`，而 Memory store 在边界内再次归一化。查询校验与归一化属于 Memory 的读取策略，不应由消费者重复准备。

## 决定

- Memory 的模型侧事实读取必须显式传入 limit：
  - `search_visible_facts_limited(query, subject, limit)` 负责查询归一化、敏感行过滤、来源片段遮蔽和限量。
  - `list_visible_facts_for_subject_limited(subject, limit)` 保留该 subject 的既有排序，先过滤再限量。
  - `top_visible_facts_by_effective_confidence(limit)` 表达跨 subject 的有效置信度排序，不再称为 recent。
- Tools 直接使用有界端口；不再接收宽结果后自行截断，也不再重复归一化查询。
- App 管理命令保留 `list_facts(source)` 的全量事实浏览能力。它是明确的管理列表用途，与模型工具的有界 search/list 不合并。
- Visibility 必须先于 limit，避免敏感事实占用可见结果名额。现有排序保持不变。SQLite 查询暂不提前截断：有效置信度在 Rust 侧计算，且敏感规则必须在限量前执行。此次限制的是跨 crate 返回集合与消费者职责，不宣称减少了 Memory 内部候选扫描量。

## 影响

不改变 SQLite schema、事实顺序、可见性规则、工具参数或输出 shape。模型工具 search/list 返回的事实和数量保持原语义；`limit=0` 的 typed store 调用直接返回空集合。移除旧 `recent` 列表、无界 subject 列表和无界搜索方法，不提供兼容别名。

## 验证

- Memory 测试验证查询归一化、subject scope、排序、敏感事实先过滤再限量，以及零 limit。
- Tools 事实 search/list 只通过显式限量 store 方法读取。
- 静态搜索确认旧无界 MemoryTool 方法与旧 `recent` 方法没有生产调用点。
- 全 workspace formatting、check、tests 与严格 Clippy；ADR 索引检查。

## 替代方案

- 保留宽查询并仅在 Tools 调整 `truncate`：拒绝，因为 MemoryTool 仍接收所有候选并承担 limit 职责。
- 在 SQL 先 limit 再过滤敏感 facts：拒绝，因为敏感行会消耗结果槽，且不能保证与既有可见排序相同。
- 将 App 的管理浏览并入模型工具限量查询：拒绝，因为 App 有意呈现完整管理集合，两者的容量/用途不同。

## 回滚

移除三个有界读方法并恢复旧方法与 Tools 截断逻辑即可。无持久化数据或 wire schema 变化，无需重置数据。
