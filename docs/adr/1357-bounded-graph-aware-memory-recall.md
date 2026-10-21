# ADR 1357：为记忆召回增加有界图谱邻居扩展

## 状态

已采纳 — 2026-10-21

## 背景

Facts 已以 SPO 形式持久化，并通过 `subject_id`、`object_id` 关联 `memory_nodes`。当前关键词与向量召回只返回直接命中的 fact；即使命中一个实体，也不会带回共享该概念节点的相邻关系。普通提示词偶尔补入用户 facts，无法覆盖面向用户的定向召回，也没有利用图边身份。

## 决定

1. 只在 fact 类召回上增加一跳邻居扩展。关键词/向量直接命中作为种子 facts，从其 subject/object `node-*` 收集概念节点，并读取与这些节点相邻的其他 facts。
2. 每次最多使用 16 个种子 facts，最多扩展 80 个候选。只扩展 `kind='concept'` 节点；忽略通用 `user` 节点，避免一个用户事实把整份画像带入结果。只读一跳，不递归扩展新命中。
3. 邻居候选排在直接关键词候选之后，再与向量结果通过现有 RRF 合并。候选继续遵守 `MemoryRecallKind` 的事实类别、精确 fact subject 约束、规范 fact identity 与共同敏感内容过滤。
4. 为 `facts.subject_id` 和 `facts.object_id` 建索引。数据库 schema 升至 v55；旧版本按发布重置文档完整备份并重置数据库。ADR 记录类型、查询语义与索引契约，重置同时清除现有会话和记忆数据。
5. 邻居命中以 `subject: predicate=object` 返回，保留关系主体上下文；既有直接命中格式不变。MemoryRecall DTO、IPC 与 embedding wire shape 不变。

## 考虑过的方案

- 只从 query 文本匹配节点标签并读取该节点的边：拒绝。节点名相同的事实往往已被文本召回，不能覆盖“从已命中的关系端点继续关联”的情况。
- 多跳遍历或 LLM 查询分解：拒绝。多跳会扩大图度数、噪声和查询成本；模型分解需要单独预算、路由与失败契约。
- 每条事实 embedding 同时承担关系扩展：拒绝。embedding 表仍只做候选排序；关系关联由规范 SPO ID 与 SQLite 索引负责。

## 后果

- 图扩展仍受查询上限、关系类别、主体范围及既有可见性策略控制；图节点不会作为可见记忆实体或新的 IPC 类型暴露。
- 高度连接的实体可能产生噪声，但节点数、候选数、直接命中优先级和一跳限制均有硬上限。
- schema v55 旧库需重置，详见 [发布与数据重置说明](../release-and-reset.md)。

## 验收

- 一个种子事实的 subject 与 object 概念节点两侧都能带回相邻事实；种子本身不会重复返回。
- 高度连接节点的候选数受上限约束；类别与 subject 过滤在图邻居 SQL 中生效。
- 敏感邻居在最终 MemoryRecall 结果中不可见；无邻居时维持原召回行为。
- 图邻居文本携带 subject；无 schema 外部 DTO 或 embedding wire 变化。
- schema index、格式、Memory 测试、strict Clippy 和 workspace 编译通过；适用测试失败与环境限制记录在此 ADR。

## 验证记录

2026-10-21：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、
`cargo clippy --workspace --locked -- -D warnings` 与 `cargo test --locked -p haven-memory`
通过；Memory 为 480 passed / 2 ignored。图谱召回用例覆盖 keyword 与 vector 种子、
subject/object 两侧、类别与主体过滤、80 候选上限、规范 ID、敏感邻居过滤及主体上下文。

`cargo test --workspace --locked` 仍失败于未改动的
`haven-platform::windows_container::hcs_process::tests::quota_probe_checks_bounded_catalog_package_trees_with_fixed_module_roots`：
测试生成的 HCS guest command line 超出 Windows 长度上限。workspace 编译和 strict Clippy
通过；该 platform 测试与本切片无依赖。
