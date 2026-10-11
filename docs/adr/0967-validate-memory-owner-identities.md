# ADR 0967：统一校验 Memory 持久化 owner 的实体身份

## 状态

已接受并实现（2026-10-11）。

## 背景

多轮契约审查已覆盖 Agent transcript、Memory message projection、用量补偿和 fact extraction 的部分身份边界，但仍有多个 Memory owner 接受任意字符串，或在读取畸形持久行时将其当作空结果。SessionStore 的事件追加/replay、FactGraph、事实查询与删除、节点、embedding、usage、授权 grant 和 ToolRun store 都承载持久实体身份，必须遵循同一 Common canonical ID contract。

## 决定

- SessionStore 在写入、分页、读取、replay 与恢复入口校验 `ses-*`；assistant Thought 与 ToolStep 使用 `step-*`，pending/user message 使用 `msg-*`。`SessionCommitted` 的 ThoughtStep 投影按来源允许 `step-*`（assistant thought）或 `msg-*`（supplement），保持同一内容行和执行行共享 ID。持久 `session_events.session_id` 行映射也校验 `ses-*`。
- Memory fact、node、usage、ToolRun、session authorization 与 embedding owner 在接收写入、查询身份和映射持久行时调用 `haven_common::types::is_canonical_id`。Fact provenance 的 message identity 使用 `msg-*`；ToolRun session/source-step 引用使用 `ses-*` / `step-*`。
- Summary extraction 的 episode identity 必须属于对应 session；节点查询遇到 SQLite 错误时向调用方传播错误，不将错误伪装成空结果。
- 对畸形持久行或外部 ID fail closed；缺失但格式正确的实体仍保留各 API 原有的 not-found/empty 语义。

## 影响

只收紧既有 Memory owner 边界，不改变 schema、event payload、IPC DTO、合法 ID 或 replay 顺序。写入会在持久化前拒绝非规范身份；读取畸形身份时返回错误，调用方不能再将其误认为有效数据或空查询。无需数据库迁移；若受支持数据中存在畸形实体 ID，按 [release-and-reset.md](../release-and-reset.md) 处理。

## 验证

- Memory repository 单测覆盖畸形输入、畸形持久行、summary/session 归属、错误传播及拒绝路径不写入。
- `cargo fmt --all -- --check`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `pwsh -NoProfile -File scripts/check-adr-index.ps1`

## 回滚

回退各 Memory owner 的 identity guards 和相关测试即可；无 schema 或有效数据迁移。
