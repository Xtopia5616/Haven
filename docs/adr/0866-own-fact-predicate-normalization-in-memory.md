# ADR 0866：由 Memory 独占事实谓词规范化

## 状态

Accepted — 2026-10-10

## 背景

事实谓词 trim、小写与别名映射由 `haven-memory::repositories::facts::normalize_predicate` 实现。Memory 的所有事实写路径都调用该实现；Agent `fact_extraction::normalize_predicate` 只是转发函数，却被 inference、worker、maintenance 与 Agent 测试当作入口。该转发使实际策略 owner 与架构文档中的模块职责不一致，也让 Agent 看起来拥有一项只属于 Memory canonical facts 的规则。

## 决定

- 删除 Agent 私有的 `normalize_predicate` 转发函数。
- Agent inference、fact persistence preparation、predicate merge maintenance 和现有调用点直接引用 Memory facts repository 的规范化函数。
- 保留 `fact_extraction` 对 LLM fact DTO、字段 coercion、tag/field sanitization 与 JSON array extraction 的所有权；canonical predicate alias map 与规则继续由 Memory facts repository 独占。
- 不改变谓词 trim、case-fold、alias mapping、调用顺序或 Memory 持久化结果；不增加新的跨 crate API。

## 替代方案

- 将别名表迁入 Agent extraction：拒绝。Memory 的用户事实、facts tool 和其它写路径也必须应用同一 canonicalization，Memory 是最低且实际的 domain owner。
- 保留 Agent wrapper 作为 facade：拒绝。它不添加参数、规则、错误或视图语义，当前 consumers 已直接依赖 Memory。
- 将谓词规范化提升到 Common：拒绝。谓词别名源于 Memory 的事实图谱与单值/衰减语义，并非通用字符串规范化。

## 影响与验证

事实谓词 canonicalization 只剩 Memory 一个实现与命名入口，Agent 的 fact extraction 职责描述与实现一致。workspace、测试目标编译及严格 Clippy 通过；测试套件未执行。无 schema、IPC、配置或持久格式变化，不需要重置。

## 回滚

如果 Agent 将来需要不同于 Memory canonical fact predicate 的模型输出规范，应为该不同语义使用明确命名的新转换，不恢复无行为转发函数。当前无持久数据需要重置。
