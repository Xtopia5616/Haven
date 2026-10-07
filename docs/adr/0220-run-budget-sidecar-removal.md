# ADR 0220：移除 run budget 的死写 actor 状态

> 本文第 3 项中保留公开 `RunBudget` 类型的决定已由 [ADR 0635](0635-unify-agent-session-run-terminology.md) 部分替代；其余决定仍有效。

- 状态：已采纳（2026-09-24）
- 范围：`haven-agent` 的 SessionActor run budget mailbox 状态
- 关联：[ADR 0214](0214-react-run-inside-session-actor.md)

## 背景

`SessionRuntimeState.run_budget` 只由 `SetRunBudget` 写入、由 `ClearRunBudget` 清空，没有读取者。ReAct 循环将预算快照发入 actor mailbox，退出时再清理；这些消息只维护了无消费者的副本。实际步数与工具重试判断一直由当前 loop 内的 `RunBudgetConfig` 完成。

## 决定

1. 删除 `SessionRuntimeState.run_budget`、`SetRunBudget` / `ClearRunBudget` 命令、handle 方法及 actor 分支。
2. 删除 ReActEngine 的预算写入和清理方法、`RunMsgIdGuard` 中对应的清理，以及 run 开始时的 mailbox 写入。
3. 当前 loop 的预算事实以本次 run 捕获的 `RunBudgetConfig` 为准；保留其步数和重试计算、公开 `RunBudget` 类型，以及既有 pause 流程和测试语义。
4. 这是 ADR 0214 的一个极小收口切片，只移除未被读取的预算副本，不代表 ADR 0214 已完整实现。其余 mailbox 命令以及 pause snapshot wire、数据库、事件和 UI 均不在范围内。

## 替代方案

- 保留预算字段和 mailbox 写入：状态没有读取者，不提供可观察能力，只增加无效的 actor 状态与消息。
- 顺带删除其他内部 mailbox 命令或改造 pause snapshot：超出本切片范围，留待对应工作处理。

## 影响与验证

- actor 不再存储 run budget 副本；循环的有效步数上限、session cap 和工具重试边界继续由 `RunBudgetConfig` 计算。
- 保留公开 `RunBudget` 类型；暂停交互与现有 pause 测试行为不变。不涉及 schema、durable event、wire 或用户数据迁移。
- 验证命令：

  ```text
  cargo fmt -p haven-agent -- --check
  cargo check --locked -p haven-agent
  cargo test --locked -p haven-agent --lib
  cargo clippy --locked -p haven-agent -- -D warnings
  git diff --check
  ```

## 回滚

回退本切片实现和本 ADR 即可恢复预算 mailbox 状态；没有数据库、事件、wire 或用户数据迁移。
