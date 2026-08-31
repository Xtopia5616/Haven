# ADR 0058：Agent 请求上下文与流式输出代次

## 背景

Run/Turn/ToolBatch 拆分后，ReAct 仍有两类容易互相污染的临时数据：provider 请求
会在多个调用点 clone、sanitize、追加 retry nudge；流式输出则由 thought/reasoning
的独立队列、provider 重试和前端归并器分别拼接。这样会产生三种错误：内部重试提示
进入后续请求、失败尝试与新尝试串成一个气泡，以及交错到达的 chunk 被按 message id
重新排序。跨会话 inbox 还把多个 envelope 先拼成一段文本，丢失消息边界。

## 决定

- `react/request_context.rs` 定义唯一的 provider 请求投影。它从 `ReActState` 的
  durable canonical 创建不可变副本，并集中处理 sanitize、一次性失败提示和截断重试
  指令；这些修复永远不回写 transcript/canonical。
- `react/context.rs` 返回有序 `PendingContextBatch`。每个 inbox envelope 是一个
  独立上下文项，低信任 framing 与字段清洗在来源边界完成；`inject.rs` 只负责经
  `apply_transcript` 投影，不再拼接或读取来源。
- `haven-llm` 暴露显式的 provider attempt 回调。failover、stream rule retry、
  empty/cut-off retry 和 compaction retry 在替换可见输出时发出边界信号。
- Agent 的 thought/reasoning chunk 共用一个有序后端队列；`agent:stream_reset` 是
  硬边界，保证旧 chunk 先完成，再清理 UI 的 live stream block。它不删除或回滚
  durable transcript，最终 thought/reasoning 投影仍是权威修复路径。
- 前端 `streamAggregator` 只合并相邻且身份完全相同的 chunk，绝不通过 Map 把非
  相邻 chunk 移到一起。reset 只清理对应输出代次，工具卡片、搜索结果和用户消息不受影响。
- Default hook 先完成 inbox 与 MEMORY fence 更新，再计算 compaction 的输入预算，
  让 compact 与随后的 `RequestContext` 使用同一份 canonical 视图。

## 替代方案

- 继续让 `turn`、compaction retry 和 stream retry 各自 clone/append/sanitize：调用点
  看似短，但请求语义无法统一，拒绝。
- 将 retry nudge 或 provider 修复写回 canonical：会把控制语句伪装成用户历史，破坏
  resume、rollback 和事实抽取，拒绝。
- 继续使用 thought/reasoning 独立队列，或在 UI 里按 message id 全局合并：无法表达
  attempt 边界，并会重排 A₁、B₁、A₂，拒绝。
- 把多个 inbox envelope 拼成一条 User 消息：短期减少消息数，但丢失 id、回复关系和
  每条消息的可审计边界，拒绝。

## 影响

新增一个 Agent → app → UI 的 `agent:stream_reset` IPC 事件；Rust wire DTO 与前端
contract 同步更新。数据库 schema、snapshot shape、events authority 和已有 durable
transcript 不变，不需要数据迁移或兼容分支。provider 的旧公共流式入口保留给非 Agent
调用者，Agent 使用带 attempt 边界的新入口。

流式队列仍有固定容量，满载时允许丢弃可由最终 snap 修复的普通 chunk；reset 标记优先
于普通事件并保持顺序。每次替换尝试同时推进 `PartialStore` 的 session generation，
并在替换前清理旧 scratch row，防止旧的异步 checkpoint 晚到后覆盖新尝试或被终态
promote。每次流结束还会等待已创建的 checkpoint task，确保最终 assistant 投影先于
scratch row 的时间戳收敛，避免成功响应在终态 promote 时被重复。取消、超时和 provider
全失败仍由原有生命周期与 partial checkpoint 路径处理。

## 验证

```text
cargo fmt --all -- --check
cargo check --workspace --locked
cargo clippy --workspace --locked -- -D warnings
cargo test --workspace --locked
cd ui
corepack pnpm run check
corepack pnpm run test:run
corepack pnpm run build
```

重点回归请求副本不污染 canonical、retry nudge 不累积、跨会话 envelope 边界、memory
patch 与 compact 顺序、attempt reset 顺序、partial checkpoint generation 隔离、chunk
丢失后的最终投影修复，以及 A₁/B₁/A₂ 交错 chunk 不重排。

## 回滚

回退本 ADR 对应提交即可恢复旧的请求拼接和流式事件路径；不涉及数据库或用户数据
重置。若只回退部分代码，必须同时回退 `RequestContext` 调用方、`stream_reset` 的
Rust/UI 契约、事件登记和前端 handler，不能留下单边 IPC 事件。
