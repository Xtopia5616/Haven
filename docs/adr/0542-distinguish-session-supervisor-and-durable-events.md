# ADR 0542：区分 SessionSupervisor 通知与持久 SessionEvent

## 状态

已采纳并实施；Rust workspace 门禁通过。

## 背景

Agent `session::SessionEvent` 是 `SessionSupervisor` 通过进程内 broadcast 发给 App 的异步通知，包含交互请求、resume/暂停/清理结果、计划确认通知和 session 错误。它不是 event-sourced 数据，不写入数据库，也不承担 replay。Memory `SessionEvent` 是 `session_events` 表的 durable 行，携带 session、sequence、event type/version、payload 和时间戳，是 resume/replay/rollback 与 commit 后发布的权威来源。Agent root 同时将 Memory 类型另名导出为 `DurableSessionEvent`，凸显出 root 上的 `SessionEvent` 仍有歧义。

## 决定

1. 将 Agent broadcast enum 改名为 `SessionSupervisorEvent`，明确生产者及进程内通知角色；更新 `SessionSupervisor` channel、订阅 API、AgentLayer 消费者与 App bootstrap。
2. Agent crate root 以 canonical 名 `SessionEvent` 重新导出 Memory durable 行，并移除 `DurableSessionEvent` 别名。
3. 保持 event channel 顺序、broadcast 语义、App mapper/Tauri events、Memory payload、sequence、数据库 schema 与恢复行为不变；两类事件 owner 不合并。

## 替代方案

- 合并为一个 event enum：拒绝。Supervisor 副作用通知不具备 durable sequence/payload，Memory durable event 行也不是 App command；合并会破坏各自生命周期和存储权威。
- 保留同名 enum 和 durable alias：拒绝。Rust 路径可消歧，但 API 阅读和搜索仍要求读者记住额外别名，不能从核心类型名看出事件来源。
- 将 Memory durable row 改叫 `DurableSessionEvent`：拒绝。它已有稳定 Memory/Agent replay API 与架构术语；由临时 Supervisor 通知承担限定词更准确。

## 影响与验证

- Rust 公共符号名变化，不涉及 Serde、SQLite、Tauri IPC/event payload 或 UI contract。无需数据库、配置或用户数据迁移。
- 验证通过：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked -- --test-threads=1`、`scripts/check-adr-index.ps1` 与 `git diff --check`。workspace 单元测试及 doc-tests 全部通过；Tools 为 799 passed、2 ignored（人工容量测试）。

## 回滚

恢复 Agent enum `SessionEvent` 和 Memory `DurableSessionEvent` re-export，并回退其订阅/匹配点及当前架构命名说明；无持久化或 wire contract 回滚。
