# ADR 0178：ReAct 可观测性与会话刷新合并

- Status: Accepted
- Date: 2026-09-19
- Owners: Haven maintainers

## 背景

ReAct 优化需要区分本地阶段耗时、SQLite writer-lock 等待、上下文队列深度和
流式 UI 的 frame/chunk 行为。聊天页和历史页还可能同时收到多个 session
lifecycle 事件，重复调用 `get_sessions` 会放大 IPC 和数据库读取。

## 决定

- ReAct metrics 使用固定大小的原子计数器、直方图和 gauge；新增
  `sqlite_lock_wait` 阶段与 context queue depth。指标不记录 prompt、密钥或工具
  输出，且不阻塞主循环。
- Transcript batch 在 `BEGIN IMMEDIATE` 后记录 writer-lock 等待时长，提交成功后
  由 Agent 观察该样本；事件与投影的事务边界保持不变。
- UI stream aggregator 暴露受限的 frame/chunk/drop 快照，用于测试和本地诊断，
  不把流式文本写入指标。
- 主聊天页和历史页的 lifecycle 刷新均使用定时合并器：显式加载立即执行，事件突发
  在当前请求结束后最多追加一次刷新；组件销毁时取消定时器。

## 验证与回滚

- 指标单测覆盖 p50/p95、SQLite 阶段、队列 gauge 和并发原子更新。
- UI 测试覆盖同一 frame 的 chunk 合并、统计快照和刷新合并器的 in-flight follow-up。
- ReAct 与 memory 测试覆盖 64-event transcript burst 的顺序与事务提交。
- 回退本 ADR 与对应代码即可，不涉及数据库 schema 或 wire payload 变更。
