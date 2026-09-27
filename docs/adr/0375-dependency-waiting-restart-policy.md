# ADR 0375：Dependency-waiting 定时任务的持久化与重启语义

- 状态：已采纳（产品决策 2026-09-26；文档登记 2026-09-27）
- 范围：scheduled action 使用 `watch_action_id` 等待另一 action 的完成状态
- 关联：ADR 0172、0343、0352、0356、0373；`docs/architecture-refactor-roadmap.md`

## 背景

既有实现通过 `ActionService` 的进程内 watcher 检查 `watch_action_id` 对应 action 状态；依赖关系不写入 durable action row，应用重启后不恢复。ADR 0343、0352 和 0356 当时把是否跨重启恢复列为未决项。产品现已确认重启与 continuation 行为，本 ADR 固化该契约。

## 决定

1. 被依赖 action 进入 `completed`、`failed` 或 `cancelled` 任一终态，都满足等待条件。continuation 接收 producer 的终态与结果。
2. 找不到 producer 时，不让 continuation 永久等待；触发一次 continuation，并传递 `not_found` 状态。
3. dependency relation 必须 durable。应用重启时根据持久关系重建 watcher；依赖满足后 continuation 只执行一次。
4. 如果 scheduled continuation 已进入 `running` 后进程崩溃，重启时将它恢复为 `failed`，不自动重放副作用；用户可手动重试。
5. UI 继续使用 `waiting`、`running` 和 terminal 三类主状态；具体等待/执行阶段可作为任务卡详情展示。

本决定不增加统一 action-level deadline、claimant owner token、lease renewal 或自动 replay，也不改变现有 action kind/status 集合。

## 备选方案

- 保持 dependency relation 仅在进程内：实现最简单，但与确认的重启行为不符，拒绝。
- 进程重启后自动重放已进入 `running` 的 continuation：无法证明外部工具副作用未发生，拒绝；由启动清理标记 `failed`，交给用户手动重试。
- producer 不存在时无限等待：会留下无法满足的任务，拒绝；按一次 `not_found` continuation 收口。

## 当前实现状态

本 ADR 只固定行为契约，**不表示实现已完成**。当前 `ActionService` 的 `watch_action_id` 关系仍是进程内状态，不写入 durable action table，重启时不重建 watcher。阶段 7 需要独立实现 durable relation、启动恢复、单次 continuation 仲裁，以及上述 running-crash 转 `failed` 行为，并补充故障与重启回归。

该实现如需演进数据库 schema，必须按 `AGENTS.md` 与 `docs/release-and-reset.md` 明确版本和重置范围；本 ADR 不变更 schema、配置、IPC 或用户数据。

## 影响与验证

静态核对 `crates/tools/src/action_service.rs` 确认当前 dependency watcher 是进程内行为，尚无 durable relation 或跨重启恢复。本 ADR 只记录产品契约，不运行 Rust/UI 门禁；后续实现必须覆盖三类 producer 终态、producer 缺失、重启重建、重复恢复、依赖满足后的单次执行，以及 `running` 后崩溃转 `failed` 且不自动 replay。

## 回滚

如产品改变该策略，应更新本 ADR、路线图与测试契约后再调整实现。回退本 ADR 本身不涉及数据操作；实现阶段产生的 schema 或 durable relation 需在其独立 ADR 中定义回滚/重置方式。
