# ADR 0062：ReAct Loop 响应周期与工具批次计划

## 背景

现有 Run/Turn/ToolBatch 已经完成第一轮拆分，但 `turn.rs` 仍把 provider
响应重试、响应投影和生命周期决策揉在同一段流程中；`tool_batch.rs` 则在执行
前同时生成 tool-call canonical、Action 卡片、step id 和确认数据。随着暂停、
恢复和并行工具继续增加，这些边界很容易出现只更新一份身份或把失败尝试写进
durable transcript 的问题。

## 决定

- 增加 `react/response_cycle.rs`。它只处理一次模型响应通过 `after_llm` 策略的
  过程；空响应/截断重试使用不可变 `RequestContext`，只有被接受的响应才能进入
  transcript 投影。取消仍返回显式的 `Cancelled`，不伪造模型响应。
- 增加 `react/tool_batch_plan.rs`。它从 assistant 的 tool-call 数组一次性过滤
  `final_answer`、生成 `step-*` 身份、分配 `action_index`，并派生 canonical
  tool calls 与 Action 卡片。执行与完成顺序可以并发/乱序，但所有持久化仍按该
  计划的顺序归并。
- 将工具批次进一步按职责拆分：`tool_batch_execute.rs` 负责验证、准入、并发执行、
  取消和按序提交，`tool_batch_policy.rs` 只负责失败分类和重试提示，
  `tool_batch.rs` 保留工具执行原语、确认生命周期和结果状态。
- Run driver 计算本次 run 的绝对终点，并只把 `allow_tool_retry` 布尔策略传入
  Turn/ToolBatch。ToolBatch 不再解释 per-run budget；暂停后恢复的 run 仍可在其
  绝对终点前为工具失败安排下一轮。
- 保持 `ReActSnapshot.events` 为唯一恢复权威，`apply_transcript` 为唯一 durable
  transcript writer；不引入 snapshot、wire 或数据库兼容层。

## 替代方案

- 继续在 `turn.rs` 内扩展 retry 分支：短期代码更少，但每个新策略都会重新引入
  “响应尚未接受却已投影”的风险，拒绝。
- 让执行 future 各自生成 step id：并行完成、确认恢复和 UI 卡片会依赖隐式配对，
  拒绝。
- 把整个 `RunBudget` 传入 ToolBatch：会让工具执行知道生命周期细节，恢复路径还
  容易误用从第 1 步开始的配置值，拒绝。

## 影响与重置

这是 Agent crate 内部控制流重构。模型 provider、工具协议、数据库 schema、
snapshot shape、Tauri 事件和用户数据均不变；不需要删除数据库或配置。重构后，
被重试但未接受的 reasoning 不再写入 durable transcript，最终接受的响应负责一次
权威投影。

## 验证

```text
cargo fmt --all -- --check
cargo check --workspace --locked
cargo clippy --workspace --locked -- -D warnings
cargo test --workspace --locked
```

重点验证 response-cycle 的取消/重试边界、工具计划的身份与顺序、并行结果归并、
暂停恢复和恢复 run 的失败重试预算。

## 回滚

回退本 ADR 对应提交即可恢复原有 Turn 内响应策略和 ToolBatch 内身份生成；不需要
数据库或用户数据迁移。
