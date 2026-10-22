# ADR 1358：为记忆检索增加 Profile、Semantic 与 Procedural 类型

## 状态

已采纳 — 2026-10-21

## 背景

Memory 已保存 SPO facts 和 episodic items，但 fact 只有 `source=user|inferred`。
来源回答“谁提供了这条信息”，不能表达“它在长期记忆中承担什么作用”。因此用户画像、
会话推断事实和可复用操作流程只能混在同一事实列表和 `kind=fact` 检索结果中。

## 决定

1. 在 facts 上增加闭合集合 `memory_kind`：`profile`、`semantic`、`procedural`。
   `source` 仍独立表达用户提供或模型推断；推断写入默认 `semantic`，用户手动记忆默认
   `profile`。手动记忆 API 可明确写入 `procedural`，但不能把它标记为推断型 `semantic`。
2. `MemoryRecallKind` 扩展 `recall_memory` 与 MemoryTool 的闭合检索类别：`fact` 继续表示全部
   facts，`episode` 表示过去对话；`profile`、`semantic`、`procedural` 分别在 SQL 候选生成前
   过滤事实类别。关键词与向量检索使用相同过滤；向量候选仍在 Memory 内按可见性规则水合。
3. MemoryTool 的 `remember` 接受可选 `memory_kind`（`profile` 或 `procedural`），默认 profile。
   长期记忆页显示类别徽标，并允许按类别检索；原有 `fact` 和 `episode` 查询继续有效。
4. Schema v54 增加 `facts.memory_kind` 约束列和查询索引。旧 schema 不做运行时迁移，按
   [发布与数据重置说明](../release-and-reset.md)备份并重置数据库。

## 后果

- `memory_kind` 是事实的语义分类；它不替代 SPO、source、confidence、durability 或 provenance。
- 用户可维护的流程仍以 fact 的 predicate/object 表达，由同一 facts owner 写入和删除；
  不引入另一张流程表或 embedding 域。
- Profile 是可检索的用户事实集合，不是自动生成的单段画像摘要。推理路由、时序版本和
  人工审核队列分别由后续独立 slice 决定。

## 验收

- 按 `profile`、`semantic`、`procedural` 查询时，关键词和向量结果都不混入其它类别。
- 未配置 embedding 时类别过滤仍由关键词查询工作。
- MemoryTool 可显式写入 profile 或 procedural，默认写入 profile。
- facts 安全过滤、原有 `fact` / `episode` 检索和 schema reset 校验继续通过。

## 验证记录

2026-10-21：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、
`cargo clippy --workspace --locked -- -D warnings`、受影响 Rust 包的完整测试
（`haven-memory` 474 passed / 2 ignored、`haven-agent` 673 passed / 1 ignored、
`haven-tools` 926 passed / 2 ignored、`haven-app-binary` 266 passed）、UI 类型检查、
记忆边界 UI 测试（17 passed）、生产构建、IPC contract/event 检查和 ADR 索引检查均通过。

UI 全量测试为 1391 passed、4 failed；
失败都在未修改的 `SessionHeader.test.ts` / `SessionToolbar.test.ts`，其断言与当前按钮
样式和工作区选择器的可访问名称不一致。记忆相关定向测试全部通过。
