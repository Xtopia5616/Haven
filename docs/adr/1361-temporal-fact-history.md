# ADR 1361：事实历史与按时点查询

## 状态

已采纳 — 2026-10-21

## 背景

facts 只保存当前行。用户修正单值事实时旧行被删除，维护改写谓词时原值被覆盖；系统无法回答某个时间点“当时知道什么”。当前 `created_at`、`last_seen_at` 和来源消息引用分别表示首次写入、最近观察与证据来源，不能被误当作事实真实有效时间。

## 决定

1. 增加 system-time 历史：历史记录回答“在 RFC3339 时点 Haven 保存了什么”，不声称事实在现实中的有效起止时间。历史边界由 SQLite 写入事务记录，不回填或推断来源消息时间。
2. 用户写入替换单值事实、facts 语义字段（subject/predicate/object/source/memory_kind）被改写，以及事实行被维护删除时，先将原行快照追加到 `fact_history`，并记录 `superseded_at`。同一事实的 mention、`last_seen_at`、置信度或标签变化不产生历史版本。
3. 默认搜索、列表、Prompt、关键词/向量召回和 FTS 始终只读当前 `facts`；历史不参与 embedding 或图扩展。MemoryTool 增加显式 `history` 操作，按指定 `at` 查询当时存在的当前/历史事实，最多返回 50 条并报告是否截断。
4. 显式 `forget`、按 ID 删除、`clear_facts` 和敏感事实清理在同一事务删除匹配历史。忘记是对所有时间范围的擦除；自动低置信度清理与普通维护删除保留快照供按时点查询。
5. 历史行继续使用当前的 `fact-*` 身份和事实字段；不增加公开实体 ID。episodic source 的外键仍在来源项被删除时置空； transcript `provenance_record_id` 沿用当前不带 FK 的 opaque 引用语义。保留事实与来源片段的行为不因 session 删除改变。
6. Schema 升至 v56；不做运行时迁移，升级前备份并重置数据库。Memory 与 Tools 通过既有 capability 接口提供查询；不改变 Tauri IPC 或现有 recall DTO。

## 考虑过的方案

- 把 `created_at` 当作事实有效时间：拒绝。多数抽取没有可靠的现实起止时间，写入时间与“事实从何时开始成立”是两种不同语义。
- 把历史行继续放入普通召回：拒绝。过时事实会和当前事实竞争，并可能重新进入 prompt、embedding 排序或图扩展。
- 用户 forget 后保留历史审计数据：拒绝。用户的显式删除必须同时删除历史副本，不能从历史查询恢复已忘记的值。
- 只在 UI 隐藏旧版本：拒绝。历史查询和隐私擦除必须由 Memory 持久化 owner 执行，不能靠调用方过滤。

## 后果

- 当前 facts 是默认记忆的唯一来源；只有显式历史查询可以读取历史快照。
- 维护修剪的事实会保留在历史中；单值用户修正后，旧值可按修正前时点查询。
- 历史随事实变更累积；reinforcement 不复制快照，查询结果有硬上限。用户 forget 与敏感清理会物理删除对应历史内容。
- session 删除后，事实与其快照仍独立保留；episode provenance 外键按现有规则变空，opaque transcript ID 与安全片段沿用当前保留行为。

## 验收

- 单值用户事实更正前的查询返回旧值，更正后的查询只返回新值。
- subject/predicate/object 等语义改写保留旧版本；confidence/mention/标签强化不制造快照。
- 普通 recall、搜索和列表仅返回当前事实；显式历史查询才可看到有效的历史快照。
- forget、按 ID 删除、清空和敏感清理清除历史副本；自动低置信度清理保留历史。
- 历史查询验证 RFC3339 输入、subject 过滤、敏感可见性、结果上限与截断标记。
- Memory/Tools 受影响包测试、workspace 编译、strict Clippy、schema reset 与 ADR 索引检查通过；未通过门禁写入验证记录。

## 验证记录

2026-10-21：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、
`cargo clippy --workspace --locked -- -D warnings`、ADR 索引检查与 `git diff --check`
通过。`cargo test --locked -p haven-memory` 为 485 passed / 2 ignored；
`cargo test --locked -p haven-tools` 为 928 passed / 2 ignored，MCP 集成测试另有
7 passed。新增用例验证更正前后快照、forget 后历史擦除、敏感清理抹除和自动修剪保留、
RFC3339、subject、结果上限及敏感过滤。

当时 workspace 测试在 `haven-platform` 有 Windows 专项用例失败；Agent（675 passed / 1 ignored）、
Memory（485 passed / 2 ignored）、Tools（928 passed / 2 ignored，MCP 集成 7 passed）均通过。
