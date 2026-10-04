# ADR 0466：拆分 SessionStore 只读历史 façade

## 状态

已采纳（2026-10-04）。

## 背景

`session_events.rs` 同时承载 append-only event、事务化 transcript projection、rollback 和只读 session 历史查询。历史查询与恢复投影共用 `SessionStore` 类型，但职责和行为边界不同；将它们放在同一文件中增加了定位成本，也使该文件超过仓库文件预算。

路线图 §5.3 将只读历史 façade 作为首个内部边界候选，并明确要求保留公开 façade、SQL owner、查询语义及事件/投影事务边界。

## 决定

1. 将 `list_history`、`latest_session_record`、`count_history`、`search_history*`、`conversation_window`、`session_resume_media`、`title_generation_context` 和对应只读 DTO 放入私有 `session_history` 子模块。
2. 在 `session_events` 中继续公开 `SessionStore` 和原有 DTO 导出，保持 Rust 调用 API、参数、返回值和序列化不变。
3. 数据库查询、过滤、排序和缓存仍由现有 `Database` repository 实现；事件 append、物化 projection、rollback 和 `session_resume_projection` 继续由 `SessionStore` 协调。

## 替代方案

- 将 SQL 或 transcript projection 一并拆开：拒绝。只读边界未证明需要改变数据库 owner 或事务协调职责。
- 改变公开查询入口或 DTO 序列化：拒绝。本次只整理模块边界，不改变调用契约。

## 影响与验证

该拆分仅改变内部源码位置。公开 façade、查询行为、缓存和序列化保持不变；数据库 schema、IPC 和持久数据均不变。

验证：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked` 与 `git diff --check`。

## 回滚

将 `session_history` 中的 DTO 与 `SessionStore` impl 移回 `session_events.rs`，并移除其模块声明和重导出即可；无需数据迁移或用户状态重置。
