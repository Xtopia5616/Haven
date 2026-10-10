# ADR 0865：共用有序非空名称列表去重

## 状态

Accepted — 2026-10-10

## 背景

`load_skill.rs` 与 `tool_catalog.rs` 各自维护一份列表循环：保留首次出现顺序、忽略空名称并按完全相等去重。两处循环机制相同，但领域规范化不同：Skill 输入需要 trim 并去除 `skill__` 前缀；目录请求的 operation/root 输入需要 trim，且由 `Option<Vec<String>>` 展平。

## 决定

- 在 `haven-tools::builtin::name_list` 中集中唯一的有序非空名称去重机制，函数名为 `ordered_unique_non_empty_names`。
- 两个调用方分别保留领域入口 `normalize_skill_names` 与 `normalize_requested_names`，在调用共享机制前完成各自输入展平和规范化。
- 共享 helper 不 trim、不改大小写、不改前缀、不排序；它只丢弃空字符串并保留第一次出现的精确字符串及其顺序。
- 该模块是 Tools 私有实现，不扩展跨 crate Common API；不改变各入口的输入来源、空输入错误或后续领域行为。

## 替代方案

- 合并两个领域 normalizer：拒绝。Skill 前缀规则与目录请求的可选向量形状不同，合并会让调用方依赖不适用的策略。
- 将 helper 提升到 Common：拒绝。当前消费者都在同一 Tools crate，尚无跨 crate 领域无关消费者证据。
- 只保留两份循环并改名：拒绝。列表顺序与去重细节仍会在两处漂移。

## 影响与验证

列表的有序、空值过滤与精确去重实现现在只有一个 owner；领域规范化仍可独立审查。workspace、测试目标编译及严格 Clippy 通过；测试套件未执行。无 IPC、配置、数据库或持久化格式变化，不需要重置。

## 回滚

若任一调用方以后需要大小写折叠、排序或保留空项，应在该领域入口显式处理；只有共享的过滤/去重契约发生变化时才调整 helper。当前无持久数据需要重置。
