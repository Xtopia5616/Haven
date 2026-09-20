# ADR 0181：ReAct turn deadline 与交互恢复边界

## 状态

已接受（2026-09-20）

## 背景

Ask/Confirm 的交互状态需要先进入 durable checkpoint 才能清除内存 gate；当
`react_state` 缺失时继续返回成功会制造“已持久化”的假象。另一方面，单独包裹
`run_turn` 的 timeout 只能丢弃 async future，不能取消 provider retry、工具批次或
仍在 SQLite native busy wait 中运行的 blocking worker。

## 决策

1. 交互快照写入在找不到 `react_state` checkpoint 时显式失败；调用方保持内存中的
   pending interaction，等待后续 durable checkpoint 成功后再清除。
2. 每个 turn 从 session cancellation token 派生独立的 deadline token，并传给 provider、
   工具批次以及 transcript/snapshot/usage 的 blocking SQLite 写入。deadline 到期时先取消
   派生 token，再由各边界将取消转换为 deadline error。
3. `Database::run_blocking_cancellable` 在 blocking worker 中登记 SQLite
   `InterruptHandle`，并将 cancellable 连接的 busy wait 限制为短窗口；连接归还池时
   恢复默认 busy timeout。interrupt 不能强杀任意 `spawn_blocking` 原生代码，因此
   非协作工具仍允许完成其外部工作，但 ReAct turn 不再等待或发起后续 provider 请求。

## 替代方案

- 缺少 checkpoint 时静默成功：会丢失 Ask gate，重启后无法判断是否仍需用户回答。
- 仅增加外层 `timeout`：无法阻止 retry delay、工具 future 或 SQLite native wait 继续占用资源。
- 强制终止 blocking thread：Rust/Tokio 不提供安全的强杀语义，可能留下半完成外部副作用。

## 影响、验证与回滚

不改变 schema version，也不需要数据迁移；现有 `react_state` 数据继续按既有 reset
boundary 管理。新增验证覆盖：缺失 checkpoint fail-closed、Ask 执行后的 executor
重启恢复、provider retry deadline、非协作 blocking tool，以及持 SQLite 写锁时的
cancellable blocking write。回滚代码即可，数据库无需回滚。
