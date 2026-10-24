# ADR 1397：用稳定 durable cursor 补齐 UI 事件并隔离 rollback

## 状态

已接受；UI 自动测试与 Windows 持续聚焦丢通知验收已通过（2026-10-25）。恢复与 rollback 并发交互验收仍开放，归属发布 Gate §5.1。

## 背景

Session 事件发布后，主窗口仍可能因 renderer 短暂丢监听而漏掉已提交 transcript。`session_events` 是权威，UI snapshot 是投影；rollback 会让旧分支的在途恢复结果失效。已有实现提供 cursor 对比和快照代次隔离，但需要覆盖 cursor 检查期间持续提交与恢复/rollback 相遇的边界。

## 决定

1. 可见聊天页定期查询活动会话的轻量 durable cursor；窗口重新获得焦点时立即再查。只有 cursor 前后稳定且与恢复 snapshot 水位一致，才应用补齐结果。
2. cursor 前进时丢弃不稳定 snapshot，下一轮从权威 event stream 重读；不从 transcript 投影反向补造历史。
3. rollback/continue 使之前发起的 resume/reconcile snapshot 失效；rollback 后先重置事件水位，再恢复当前分支。晚到的旧 branch event 与 snapshot 都不能覆盖新投影。
4. `session_events` 的 branch cursor 与投影 cutoff 继续由既有双时钟契约管理；本 ADR 不改变数据库或事件格式。

## 验证

- UI 测试覆盖漏收事件补齐、稳定 cursor 校验、cursor 在 snapshot 获取期间前进时拒绝结果并在下一轮恢复、窗口持续可见轮询及重新聚焦立即补查、rollback 使旧恢复结果失效。
- 本轮 `corepack pnpm exec vitest run --maxWorkers=4` 全量 UI 测试 169 个文件、1499 项通过；`corepack pnpm run check` 为 0 错误、0 警告。
- 隔离 Windows profile 的 Haven 窗口保持前台可见时，通过独立测试进程直接向隔离数据库追加 seq 13 durable event（不发送 UI 事件）；reconciler 从 cursor 12 检出前进，读取稳定水位 13 的恢复快照并更新 reducer。该用例验证了持续聚焦时漏通知后的补齐。
- 恢复读取与 rollback 并发的真实 UI 交互尚未执行；自动测试覆盖旧快照失效，但不能替代该 Windows 验收。

## 影响与回滚

没有持久数据、schema、配置或 IPC 变化，无需重置。若回滚测试或调度变更，继续保留既有 cursor/rollback fence；删除轮询需另行证明 renderer 重订阅能完整补齐。
