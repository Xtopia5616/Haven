# ADR 0637：统一 ReAct response policy 输入与判定术语

## 状态

已采纳并实施。

## 背景

ReAct `hooks.rs` 声明 `AfterLlmInput`，作为响应 hook 与生产 classifier 之间的输入；`ResponsePolicy::classify` 又把相同的 thought、tool calls、LLM response 和 `ResponsePolicyState` 拆成四个参数。两处描述的是一个 response-policy input contract。

classifier 的结果叫 `AfterLlmAction`，但其变体是 Accept、RetryIncompleteToolArgs 和 Fail：它表示策略判定，实际 retry 与 terminal cycle outcome 由 `response_cycle` 执行。模块文件 `retries.rs` 也不完整，因为 policy 还接受正常响应并把异常/空响应转成可恢复失败。

`ResponseCycleOutcome` 位于更后阶段，包含 Accepted、Cancelled 和 RecoverableError；它承载执行 policy/retry 后的 cycle 终态，和 policy 的一次判定不是同一状态。

## 决定

1. 将 hook 和 classifier 共用的输入契约统一为 `ResponsePolicyInput`，并由 `ResponsePolicy::classify(&ResponsePolicyInput)` 接收，删除同一输入形状被拆开的第二份函数契约。
2. 将 `AfterLlmAction` 改名为 `ResponsePolicyDecision`，明确它是接受、结构参数重试或失败判定，不是已经执行的动作。
3. 将模块文件与 module 名从 `retries.rs` / `retries` 改为 `response_policy.rs` / `response_policy`，覆盖完整接受/重试/失败分类职责。`ResponsePolicyState` 继续单独表达 retry 次数与 pending ask 状态。
4. 保留 `ResponseCycleOutcome` 与其 `AcceptedResponse`：它们是执行策略和可能的 retry 之后的 cycle 结果。响应判定、retry 边界、错误持久化和取消语义不变。

## 替代方案

- 只改 `AfterLlmAction`，保留两套输入表达：拒绝。hook contract 与 classifier signature 仍重复同一 response-policy shape。
- 将 `ResponsePolicyDecision` 与 `ResponseCycleOutcome` 合并：拒绝。前者可能继续触发重试，后者只表示整轮响应策略完成后的终态。
- 保留 `retries.rs`：拒绝。文件职责还包含响应接受与异常响应分类，名称遗漏了主要行为。

## 影响与验证

- 仅重命名 `haven-agent` 内部 React policy 类型与模块，并合并同形的输入契约；不影响 provider API、transcript、事件、wire、配置或持久数据。
- 更新生产 hook、response cycle、测试与命名路线图，确认旧类型名和模块名不在当前源代码中。
- 验证：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、ADR 索引与 staged diff 检查。

## 回滚

恢复 `retries.rs`、`AfterLlmInput`、`AfterLlmAction` 及 classifier 原有参数列表，并同步其全部调用点；不需要数据或 wire 回滚。
