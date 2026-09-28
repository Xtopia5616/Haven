# ADR 0392：持久化定时任务依赖与重启恢复实现

- 状态：已采纳并实现（决策 2026-09-28；实现 2026-09-29）
- 范围：ADR 0375 的 durable dependency relation、watcher restart recovery、单次 continuation 及 producer 结果交接
- 关联：ADR 0305、0317、0325、0332、0334、0343、0352、0373、0375；`docs/release-and-reset.md`

## 背景

ADR 0375 已确认 dependency-waiting 的产品行为，但当时 `watch_action_id` 仅保存在进程内 `ActionService` entry。应用重启后无法重建 watcher；scheduled tool producer 的实际结果也未保存在 action row，不能交给依赖 continuation。本 ADR 记录该行为的 durable 实现及与既有 terminal transcript 投影的衔接。

## 决定

1. 将 `watch_action_id` 作为 scheduled action 的持久触发关系，并与 `due_at` 互斥；新建或恢复的 scheduled action 必须恰有一个 timer 或 dependency trigger。
2. 启动时先将上个进程遗留的所有 running action 标记为 failed，再查询并恢复 waiting scheduled rows。running continuation 不自动 replay；依赖 watcher 从 durable producer 状态重建。
3. timer 与 dependency fire 都复用既有 durable `Waiting → Running` CAS。竞争的 watcher/service 只有 CAS 成功者可以发布 continuation；无接收者时沿用现有 rollback/requeue 行为。
4. producer 的 completed、failed、cancelled 都满足依赖；producer 不存在时也只 fire 一次并把 `not_found` 传给 continuation。completed/failed 状态及有结果时将其加入 continuation prompt。终态 prompt 将 action id、状态和结果统一编码为 JSON 放在不可信数据边界中，并转义 `<`，防止 producer 输出或 watch id 关闭该边界或伪装成指令。
5. scheduled tool 的成功摘要或失败原因使用既有 `notification_summary_chars` 上限截断后写入唯一 `actions.result_summary` 列。依赖 watcher 与 transcript completion 共用此有界结果，不复制原始工具输出。scheduled tool 的 completed/failed terminal row 与 `action_completion_outbox` 在同一事务写入；重启 reconciliation 可从 terminal row 重建缺失 outbox。稳定 `action_result_id` 使用 action id。Agent 复用 background 的统一 Action result envelope 与 X12 投影路径，transcript 投影成功后才 ack；cancelled 不产生 result transcript。
6. Scheduled Continue 仍通过既有 session input 路径记录 continuation，不再产生第二条 terminal result。无 owner、owner session 已删除、以及重复投影遵循既有 action-result ack 与稳定消息 ID 规则，不增加 UI event。
7. 不增加 action-level timeout、owner token、lease renewal、自动 replay 或新的 action status/kind；background 与 scheduled 保留各自 execution owner。

## 影响与重置

`actions` 新增 nullable `watch_action_id` 与 `result_summary`，数据库 schema 升至 v30。本版本不对旧数据库做运行时迁移；更新后须按 `docs/release-and-reset.md` 删除旧 `haven.db` 并重建。`result_summary` 是依赖 continuation 与 terminal transcript result 的唯一持久结果来源，仍受已有通知摘要长度上限约束；按 action row 的既有访问边界读取，不进入普通 action history/UI DTO 或日志。

恢复只扫描 `waiting` scheduled rows；启动时先将遗留 running actions 标为 failed，再重建依赖 watcher。producer terminal 后 dependency watcher 复用持久 status/result 查询。重复恢复和多 watcher 竞争由 scheduled fire CAS 仲裁，不另加 claim identity。依赖 fire 成功进入 `running` 后若进程崩溃，重启将其标记 failed，不会再次执行工具或 continuation。ActionCompletion 的 transient publish 丢失时，outbox claim/reconciliation 以相同 action_result_id 恢复投影；Agent 在 durable 投影后 ack。

## 验证

回归覆盖 durable relation/result 读写、timer/dependency trigger 互斥、terminal producer 结果交接、missing producer、producer waiting 与 cancelled、restart stale-running 先失败、两个恢复实例单次 CAS、terminal outbox 重建与 claim/ack、scheduled completed/failed 共享 result transport、无 owner/cancelled/Continue 分支、稳定 action_result_id 与 transcript 幂等。实现通过 `cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings` 和 `cargo test --workspace --locked`。

## 回滚

回滚实现前需停止 Haven 并按 `docs/release-and-reset.md` 删除 v30 `haven.db`；旧二进制不支持该 schema。若改变依赖恢复或 terminal result 投影语义，应先更新 ADR 0375 与本 ADR，再调整状态转换及回归测试。
