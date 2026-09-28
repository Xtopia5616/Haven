# ADR 0392：持久化定时任务依赖与重启恢复实现

- 状态：已采纳（2026-09-28）
- 范围：ADR 0375 的 durable dependency relation、watcher restart recovery、单次 continuation 及 producer 结果交接
- 关联：ADR 0305、0317、0325、0332、0334、0343、0352、0373、0375；`docs/release-and-reset.md`

## 背景

ADR 0375 已确认 dependency-waiting 的产品行为，但 `watch_action_id` 仅保存在进程内 `ActionService` entry。应用重启后无法重建 watcher；scheduled tool producer 的实际结果也未保存在 action row，不能交给依赖 continuation。

## 决定

1. 将 `watch_action_id` 作为 scheduled action 的持久触发关系，并与 `due_at` 互斥；新建或恢复的 scheduled action 必须恰有一个 timer 或 dependency trigger。
2. 启动时先将上个进程遗留的所有 running action 标记为 failed，再查询并恢复 waiting scheduled rows。running continuation 不自动 replay；依赖 watcher 从 durable producer 状态重建。
3. timer 与 dependency fire 都复用既有 durable `Waiting → Running` CAS。竞争的 watcher/service 只有 CAS 成功者可以发布 continuation；无接收者时沿用现有 rollback/requeue 行为。
4. producer 的 completed、failed、cancelled 都满足依赖；producer 不存在时也只 fire 一次并把 `not_found` 传给 continuation。completed/failed 状态及有结果时将其加入 continuation prompt。终态 prompt 将 action id、状态和结果统一编码为 JSON 放在不可信数据边界中，并转义 `<`，防止 producer 输出或 watch id 关闭该边界或伪装成指令。
5. scheduled tool 的结果使用既有 `notification_summary_chars` 截断后写入私有 `dependency_result` 列。该列只由依赖 watcher 读取，不加入 action history/UI DTO、生命周期事件或通知。scheduled tool failure 沿用既有 `error_reason`；cancelled 只传状态。
6. 不增加 action-level timeout、owner token、lease renewal、自动 replay 或新的 action status/kind；不改变 transcript 投影或跨 kind terminal completion 格式。

## 影响与重置

`actions` 新增 nullable `watch_action_id` 与 `dependency_result`，数据库 schema 升至 v29。本版本不对旧数据库做运行时迁移；更新后须按 `docs/release-and-reset.md` 删除旧 `haven.db` 并重建。scheduled tool summary 受已有通知摘要长度上限约束；它作为私有 action result 持久化，以供有权执行该 continuation 的 Agent 读取。

恢复只扫描 `waiting` scheduled rows；producer terminal 后 dependency watcher 复用持久 status/result 查询。重复恢复和多 watcher 竞争由 scheduled fire CAS 仲裁，不另加 claim identity。依赖 fire 成功进入 `running` 后若进程崩溃，重启将其标记 failed，不会再次执行工具或 continuation。

## 验证

回归覆盖 durable relation/result 读写、timer/dependency trigger 互斥、terminal producer 结果交接、missing producer、producer waiting 与 cancelled、restart stale-running 先失败、两个恢复实例单次 CAS，以及 scheduled action result 不进入普通 output projection。实现门禁与提交记录见本 ADR 对应提交。

## 回滚

回滚实现前需停止 Haven 并按 `docs/release-and-reset.md` 删除 v29 `haven.db`；旧二进制不支持该 schema。若改变依赖恢复语义，应先更新 ADR 0375 与本 ADR，再调整状态转换及回归测试。
