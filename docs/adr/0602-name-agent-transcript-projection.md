# ADR 0602：命名 Agent transcript projection 结果

## 状态

已采纳并实施。

## 背景

`project_transcript` 与 `project_transcript_with_strategy` 从权威的 `TranscriptRecord` event log 一次遍历派生两种视图：provider-neutral canonical messages 供模型请求使用，`ReActRound` 用于 Agent 步骤历史与 session-scoped tool 恢复。公开 crate API 原返回 `(Vec<CanonicalMessage>, Vec<ReActRound>)`，resume、rollback、ReAct 和测试通过位置解构或 `.0` / `.1` 选择输出。

## 决定

1. 返回类型改为公开的 `TranscriptProjection { canonical_messages, react_rounds }`，并从 Agent crate root re-export。
2. 两个视图继续由同一纯投影遍历生成；消费者按字段取所需内容，不引入第二次事件遍历。
3. `EventProjection` 测试辅助也返回该结构，所有生产与测试调用点移除 tuple 解读。

## 替代方案

- 分开运行两个投影函数：拒绝，两种视图读取同一事件流且共享排序/媒体选择规则，重复遍历会复制算法和成本。
- 合并 canonical message 与 ReAct round 为统一消息类型：拒绝，它们有不同形状和消费者；canonical 是模型上下文，round 包含步骤号、thought 与工具记录，不能相互替代。
- 保留 tuple 并局部命名解构变量：拒绝，公开 API 与每个调用点仍需依赖固定位置。

## 影响与验证

- 改变 `haven-agent` 的公开 Rust crate API 返回类型；workspace 内所有调用点已迁移。无 Tauri IPC、事件、数据库或配置契约变化。
- event authority、排序、media strategy、resume/rollback 以及 per-session tool restore 行为不变；命名路线图 §5.7 继续保持 Active。
- 验证：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`、ADR 索引及 staged diff 检查。

## 回滚

恢复两个投影函数返回 `(Vec<CanonicalMessage>, Vec<ReActRound>)`，并还原调用点解构；无持久化迁移。
