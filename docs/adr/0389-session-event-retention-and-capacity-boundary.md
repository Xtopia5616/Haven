# ADR 0389：Session 事件保留与容量边界

- 状态：已采纳（2026-09-28）
- 关联：ADR 0196（Session event sourcing）、ADR 0360（核心流水线性能基线）、ADR 0374（Session retention typed port）

## 背景

`session_events` 是 session 恢复与回滚的 durable authority。compaction 会改变正常恢复读取的 active root，但不会删除 root 之前的事件；rollback 也通过追加 marker 保留完整时间线。schema 有更新拒绝触发器和 session 级联删除外键；repository 不提供独立历史事件裁剪入口，因此 append-only 的边界是单个仍保留的 session。

当前应用另有 whole-session age retention：`memory.history_retention_days` 默认 90 天，设为 0 时禁用清理；应用启动后安排一次后台清理，之后每日清理按 `sessions.created_at` 过期的 session。删除 session 会级联删除该 session 的事件。它是可配置的时间保留策略，不是每会话或数据库字节上限。

ADR 0360 的 100k 事件基线使用内存 SQLite，仅测 replay 读取；它没有测数据库/WAL 的磁盘增长。transcript 的 4 MiB 上限约束单次提交批次，也不是累计事件容量限制。当前没有归档、字节容量告警或低磁盘 durable append 专项测量。

## 决定

1. 事件历史在其所属 session 保留期间完整保留。compaction 和 rollback 不独立裁剪 `session_events`；对单个 session 来说事件流 append-only，但产品不承诺事件永久保留。
2. 当前删除边界是整个 session 的既有 retention 或用户显式删除。默认 90 天、`0` 禁用和后台清理行为保持不变；删除 session 时事件行按外键级联删除。
3. 现有 retention 不构成存储上限：不限制单 session 累计事件数/字节数，也不限制数据库/WAL 总大小。ADR 0360 的内存读取数据不得用作磁盘增长或容量承诺。
4. 在稳定发布前，阶段 9 必须完成独立的存储容量设计：用磁盘数据库和代表性 payload 测量增长与清理后的文件/WAL 行为，确定是否需要字节上限、归档/导出或容量告警，并验证低磁盘时 durable append/事务失败的可观测性与恢复行为。在该设计落地前，不对上述能力作出产品承诺；不得以 compaction 名义绕过现有恢复/回滚事件契约。

## 替代方案

- 承诺永久保留全部事件：与现有 session 删除及可配置 retention 行为不符，也没有容量证据支撑。
- 立即按 compaction root 裁剪事件：会改变 rollback 和完整时间线读取能力，且没有归档/恢复设计。
- 现在直接设定固定事件或数据库字节上限：缺少磁盘增长、payload 分布和低磁盘写入数据，无法可靠设定数值。

## 影响与验证

- 这是对现有语义和未覆盖风险的决策记录，不改变 runtime、schema、配置或用户数据。
- 已核对 schema 的 append-only/update guard 与级联外键、`history_retention_days` 默认值和 0 值禁用分支、启动/每日清理入口、transcript 批次上限，以及 ADR 0360 内存 SQLite 测量边界。
- 本 ADR 不声称低磁盘 append 已有专项保护或回归；该项属于阶段 9 的容量设计与验证工作。

## 回滚

撤销本 ADR 与路线图中的容量跟踪说明即可；无 schema、配置或用户数据变更。
