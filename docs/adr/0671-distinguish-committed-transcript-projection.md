# ADR 0671：区分 Agent transcript projection 的读取与提交阶段

## 背景

公开 `haven_agent::TranscriptProjection` 是从权威 `TranscriptRecord` event log 一次派生出的两种只读视图：模型使用的 canonical messages 与 Agent 历史使用的 ReAct rounds（ADR 0602）。

`react::transcript` 内部另有同名私有结构，字段却是一个已提交 `TranscriptRecord` 与可选的已持久化媒体记录；它由 event store commit 后传给 `ReActState` 应用逻辑。两个类型处于不同模块，形状、来源阶段和消费者都不同，但同名使调用点无法辨认它属于读模型投影还是提交后应用包。

## 决定

- 私有结构改名为 `CommittedTranscriptProjection`，应用方法改为 `apply_committed_transcript_projection`，显式标明它来自 durable commit 并用于后续内存投影。
- 保留公开 `TranscriptProjection` 作为事件日志派生的 canonical messages / ReAct rounds 结果；两者不合并。
- 不改变事件追加、物化事务、媒体记录、UI 发布顺序、canonical state、IPC、配置或数据库。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --locked -p haven-agent`
- ADR 索引检查与 `git diff --check`
- 未运行测试；本轮只执行格式与编译门禁。

## 回滚与重置

恢复 Agent 内部结构与方法的旧名称即可回滚。无持久化、配置或 wire 变化，不需要重置。
