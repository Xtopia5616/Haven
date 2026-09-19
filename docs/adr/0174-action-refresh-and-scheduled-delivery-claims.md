# ADR 0174：Action 刷新一致性与 scheduled delivery claim

## 状态

已接受（2026-09-19）

## 背景

Action board 同时接收 `list_actions` 刷新结果和生命周期事件。仅按刷新请求序号
去重时，刷新开始后到达的 `action:updated` 仍可能被旧快照覆盖。另一方面，scheduled
fire 会同时保留在恢复 map 和 broadcast buffer；每个 receiver 独立维护去重集合时，
未来新增多个 scheduled consumer 会重复执行同一任务。

损坏的 `waiting` scheduled row 还可能因为数据库短暂不可写而无法隔离。若只记录一次
日志，该行会继续被 pending 查询返回，却没有可挂载的运行时 payload。

## 决策

1. UI `actionStore` 为生命周期写入维护独立状态版本。`refreshActions()` 只有在请求序号
   和请求开始时捕获的状态版本都未变化时，才可替换 live board。
2. malformed scheduled row 的隔离先执行短重试；短重试失败后为每个 action 保留一个随
   `ActionService` shutdown 取消、指数退避且上限 30 秒的后台重试任务，直到行离开 waiting
   或隔离成功。
3. `pending_scheduled_fires` 是恢复来源，scheduled completion receiver 在返回 fire 前
   必须取得 `ActionService` 级共享 claim。claim 具有 15 分钟 lease，在终态确认时释放；
   消费者崩溃后 lease 到期，后续 receiver 才能重新领取。后台 completion consumer 使用
   不领取 scheduled fire 的专用接收路径。

## 替代方案

- 只增加刷新请求序号：无法覆盖刷新与生命周期事件交错到达的场景。
- 隔离失败后永久保留日志：会把 durable waiting row 变成不可见、不可取消的幽灵任务。
- 依赖每个 receiver 的本地 `seen` 集合：无法在多个 scheduled consumer 之间建立唯一领取
  事实来源。
- 强制单一 scheduled consumer：当前 Agent 确实只有一个消费者，但不能保护未来的调用方
  或测试 fixture；共享 claim 更适合保留恢复能力。

## 影响、验证与回滚

新增的状态版本和 claim/lease 均为进程内状态，不改变数据库 schema 或 IPC payload。lease
只提供消费者崩溃后的恢复窗口；跨重启仍按既有 durable `running` at-most-once 规则处理。
验证覆盖 UI 刷新竞态、隔离重试、同一 scheduled fire 的跨 receiver claim，以及现有 action
状态机、恢复、取消和数据库失败测试。

回滚代码即可；无 schema 重置要求。
