# ADR 1093：回收已完成的 App 任务句柄

## 状态

已接受（2026-10-13）

## 背景

`ApplicationRuntime` 将每个 App 级异步任务句柄保留到 shutdown。热键与托盘等高频入口会持续创建短任务，完成的句柄仍占用注册表，并令 shutdown 遍历量随应用使用时长增长。

## 决定

- 由 `JoinSet` 继续统一拥有 App 级任务。
- 每次注册新任务前收割已经完成的任务结果并记录失败；仅保留仍运行的任务供 shutdown 取消与 join。
- shutdown 继续取消所有 app-scoped 子 token，并 join 仍在运行的任务；超出既有 grace period 后 abort 并 drain。

## 影响

注册表大小随同时运行的任务数变化，不随历史任务总量增长。正常完成和 panic 的 join 结果在下一次任务注册时收割；最后一批完成任务仍由 shutdown 收割。

## 验证

AppState 回归测试反复注册短任务，再注册一个长任务，验证完成结果被收割、活跃任务仍在 owner 中，并在 shutdown 时取消。

## 回滚

不得改回永久保留已完成句柄的无界向量；任何替代 owner 必须保留完成结果诊断和 shutdown join。
