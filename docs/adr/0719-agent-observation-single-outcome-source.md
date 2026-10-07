# ADR 0719：Agent observation 使用唯一执行 outcome 来源

## 状态

已采纳并实施。

## 背景

`AgentEvent::Observation` 同时携带顶层 `outcome: String` 和 `result: ToolResultEnvelope`。两者都从同一个 `ToolExecutionOutcome` 派生：顶层用 `as_str()` 缩写超时值为 `timed_out` / `unknown`，envelope 保留 `timed_out_and_terminated` / `timed_out_unknown`。`StoredObservationUi` 也重复保存顶层字段，前端 mapper 校验两份值并优先用顶层值渲染卡片。

这形成两个执行结果来源，测试还能构造彼此矛盾的值：顶层 outcome 表示成功，而默认 `ToolResultEnvelope` 表示失败。恢复历史另外从 `session_steps.status` 读取展示状态，那是持久投影，不应成为 live event 的第二个结果来源。

## 决定

- `ToolResultEnvelope.outcome` 是 Agent observation event 的唯一执行 outcome 来源；删除 `AgentEvent::Observation`、App event DTO 和 `StoredObservationUi` 的顶层 `outcome` 字段。
- UI contract 不再定义或校验缩略的 `ToolObservationOutcome`；Session reducer 不把结果复制到消息顶层，`ToolResultCard` 从 envelope 读取并只在展示边界映射为 `completed`、`timed_out` 或 `unknown` 等卡片状态。
- 内部执行与重试决策保留其 typed `ToolExecutionOutcome`；session step 写入和恢复读取 `session_steps.status` 的职责不变。
- `observation` 仍承载模型可见的操作输出；`result` 仍仅承载稳定执行 metadata，不为动态工具输出增加 schema。

## 替代方案

- 保留两个 outcome 并要求它们相等：拒绝。相等性校验仍维护重复契约，且缩写值继续模糊类型语义。
- 将顶层字段改名为展示状态：拒绝。展示状态可由 canonical envelope 在 UI 边界派生，不需要再经过 IPC 和 committed UI 持久化。
- 把 `session_steps.status` 当作 observation event 来源：拒绝。它用于恢复历史卡片，不替代 live transcript 的 execution metadata。

## 影响与验证

Agent observation 的 Tauri payload 删除顶层 `outcome`，前端只消费 `result.outcome`。超时结果在卡片中继续显示原有标签，状态转换集中在 `ToolResultCard` 展示边界。`StoredObservationUi` 去掉重复值；canonical `session_events` / transcript 记录及 SQL schema 不变，envelope 中保留完整 outcome，因此无需数据库迁移或重置。

验证通过：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`；UI `corepack pnpm run check`（0 errors、0 warnings）、`corepack pnpm run test:run`（124 files、988 tests）与 `corepack pnpm run build`；`scripts/check-ipc-contracts.ps1`（81 handlers）、`scripts/check-ipc-events.ps1`（35 channels）、ADR index（702 records）及 `git diff --check`。

## 回滚

若需要回滚，同时恢复 Agent event、committed UI projection、App event DTO 与 UI contract 的顶层 `outcome`；canonical transcript 和数据库 schema 无需回滚或重置。
