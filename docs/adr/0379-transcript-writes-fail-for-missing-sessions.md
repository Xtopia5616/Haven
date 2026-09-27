# ADR 0379：缺失会话的 transcript 写入直接失败

- 状态：已采纳（2026-09-27）
- 范围：SessionEventStore 的 transcript append/commit ports 与 ReAct transcript append
- 关联：[ADR 0336](0336-react-session-committed-submission.md)、[ADR 0374](0374-typed-session-cleanup-and-explicit-agent-tool-wiring.md)

## 背景

`append_transcript_async` 和 `commit_transcript_cancellable` 在写入前查询 session 行；行不存在时返回 sequence `0` 或空 `SessionCommitResult`。这是为无持久化 session 的合成 ReAct 测试/旧 live loop 保留的兼容行为，会把失效 session 上的内容静默当作成功。`session_events.session_id` 已有 `sessions(id) ON DELETE CASCADE` 外键。

## 决定

1. 删除上述缺失 session 的成功空结果分支和查询；统一执行 transcript append/commit，由 session 外键拒绝不存在的 session。
2. ReAct 和 store 测试改为断言缺失 session 返回错误且没有事件或投影副作用。
3. 不改合法 session 的 event sequence、投影事务、live broadcast 或删除 cascade。

## 影响、重置与验证

- 没有 schema、数据格式、配置或 IPC 变化，不需要用户数据重置。
- 运行 Agent 和 Memory crate 测试，以及 workspace 编译、Clippy 与全量测试。

## 回滚

恢复缺失 session 的预查询和空结果行为及测试；无需数据迁移。
