# 1145：限制 VAD 命令积压并由 Job Object 取消进程树

## 状态

已采纳并实施。

## 背景

输入 callback 与 VAD worker 之间若用无界通道，排队延迟会随负载累积且无法给出内存上限。后台命令取消曾调用外部 `actionkill` 进程树工具；该工具依赖 PATH 和额外 child 生命周期，无法作为受管取消边界。

## 决定

- VAD command 队列容量固定为 2，响应队列容量固定为 1；提交与 worker 消费使用有界同步语义，pipeline reset 与 capture 生命周期明确 await。
- shutdown 先关闭 response receiver，再 join worker；已取消录音留下的 stale reply 不会让 worker 永久阻塞在有界 `blocking_send`。
- Windows 后台 child 由既有 Job Object containment owner 约束；取消调用 Tokio child kill 并释放 Job Object containment，以终止整棵进程树。不再启动额外的 `actionkill` 命令。
- shutdown/cancel 后先停止 child/descendants，再标记 ToolRun cancelled；失败仍通过 ToolRun 稳定错误出口报告。

## 影响

排队占用有固定上限，取消路径移除外部命令查找与 PID 管理；不新增持久状态。Windows Job Object 行为仍须按项目 Windows 发布验收 gate 实测。

## 验证

测试覆盖 VAD worker 取消后 response send 被关闭并可及时 join，以及后台任务取消状态；运行 Rust workspace、严格 Clippy。Windows 子进程终止的最终验收记录在独立 Windows gate。

## 回滚

无持久数据重置要求。不得恢复无界 channel 或 PATH helper；调整容量需提供可解释上限和队列压力/边界证据。
