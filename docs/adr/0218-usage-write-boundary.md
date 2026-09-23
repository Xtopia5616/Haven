# ADR 0218：Usage 写入边界收口

- 状态：已采纳（架构降复杂度路线图阶段 3 安全收口切片）
- 日期：2026-09-24
- 范围：`haven-memory` 的 usage 持久化入口
- 关联：[ADR 0206](0206-session-store-usage-events-and-atomic-rollback.md)、[架构降复杂度重构路线图](../architecture-refactor-roadmap.md)

## 背景

Usage 事实必须进入 append-only `session_events`，由 `usage_recorded` 事件驱动 `llm_usage` 与 `session_usage` 投影。旧的 `Database::persist_llm_call_*` 系列会直接写入这些投影，绕过 `SessionStore` 事件边界；生产构建已经没有这些方法的生产调用者，但入口仍可被生产代码调用。

## 决定

1. `SessionStore::append_usage` / `append_usage_batch` 是线上 usage 写入入口：在同一事务追加 `usage_recorded` 事件并将其投影到 usage 表。
2. `Database::persist_llm_call_and_refresh_session_usage`、其 cache accounting/context/kind 变体，以及 batch 变体均标记为 `#[cfg(test)]`，只作为 memory 测试夹具保留。测试构建仍可调用它们，以覆盖投影和清理行为。
3. 不删除这些测试入口；不改已是 `#[cfg(test)]` 的 `record_llm_call_usage` 系列，也不改读 API、事件 API、`SessionStore::append_usage`、schema 或数据格式。
4. 本切片只移除生产构建中的旧直写能力，不完成 typed `UsageStore` 或 enum 化；后续阶段再独立处理该类型边界。

## 替代方案

- 保留公开 Database 直写方法：会继续提供绕过 append-only 事件与投影原子边界的生产写路径。
- 删除旧方法及其测试：会失去现有投影行为测试夹具，且超出本切片要求。
- 本切片同时引入 typed `UsageStore`：会扩大改动面，将类型重构与生产写入口收口混在一起。

## 影响与验证

- 生产构建中这五个旧 Database 写方法不可用；usage 的线上写入通过 `SessionStore` 事件投影。
- 测试构建仍保留方法及其调用，不改持久化格式、数据或 schema；无需重置用户数据。
- 全仓调用审计确认，直接调用位于 `usage.rs` 与 `messages.rs` 的 `#[cfg(test)]` 模块；定义之间的委托也仅服务于这些测试夹具。ADR 0176 中有一个历史名称引用，不是代码调用。
- 验证命令：

  ```text
  cargo fmt -p haven-memory -- --check
  cargo check --locked -p haven-memory
  cargo test --locked -p haven-memory --lib
  cargo clippy --locked -p haven-memory -- -D warnings
  git diff --check
  ```

## 回滚

移除五个方法上的 `#[cfg(test)]` 并删除本 ADR 与索引项即可。没有 schema、数据格式或用户数据变化，回滚无需数据库重置。
