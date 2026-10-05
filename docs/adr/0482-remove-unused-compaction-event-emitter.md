# ADR 0482：移除未调用的 Compaction 直发 helper

## 状态

已采纳并实施（2026-10-05）。

## 背景

ADR 0336 规定 ReAct live transcript 先提交到 `SessionStore`，成功后由 `CommittedUiPublisher` 按 durable event sequence 发布 Thought、Action、Observation、Supplement、MediaPlan 与 Compaction。`crates/agent/src/react/committed_ui.rs` 已将 `TranscriptRecord::CompactSummary` 映射为带 `event_seq` 的 `AgentEvent::Compaction`。

`EventDispatcher::emit_compaction_from` 和它专属的 `CompactionEventData` 没有仓库内调用方，也没有测试覆盖它。该 helper 可以独立向 emitter 发送 Compaction 事件，使旧的或未来的调用绕过 committed transcript 的单一发布路径。`event` 是 Agent 内部私有模块，该参数类型没有从 crate 根导出，因此不存在已使用的外部调用契约。

## 决定

1. 删除未调用的 `EventDispatcher::emit_compaction_from` 与仅供该方法使用的 `CompactionEventData`。
2. 保留 `AgentEvent::Compaction`、序列化 payload 与 `CommittedUiPublisher` 的现有实现。已提交的 `CompactSummary` 仍携带其 `session_events.sequence` 发布；无持久化的 compaction 旁路不再提供专用 helper。
3. 不拆分 `event.rs`，不改 SessionStore 事务、不改 UI event/wire、schema 或运行时业务行为。

## 替代方案

- 保留 helper 以供潜在调用者使用：拒绝。仓库内没有使用点，且参数类型不可从 crate 根访问；保留只会留下与 durable owner 决定冲突的发布入口。
- 将 helper 搬到 `CommittedUiPublisher` 之外的另一个模块：拒绝。那会保留第二条 compaction 事件生产路径。

## 验证与影响

删除后代码中不再出现 `CompactionEventData` 与 `emit_compaction_from`；ADR 与路线图保留名称用于记录审计结论。现有 `react::transcript` compaction 测试继续验证已提交 summary 发布的 `event_seq` 与 durable sequence 相同。运行 workspace 格式、测试、check、严格 Clippy 与 `git diff --check`。

无数据库、配置、IPC、序列化或用户数据迁移影响。外部 Agent event variant 不变。

## 回滚

恢复该 helper 与参数类型会重新开放不经过 committed transcript 的 Compaction 发布入口；只有新增一个明确符合 ADR 0336 的真实使用场景并经单独 ADR 复核后才应回滚。
