# ADR 0578：为 Session 子 reducer 命名 action 子集

## 状态

已采纳并实施。

## 背景

Session reducer 将共享的 `SessionAction` 按标签分发到 Agent、Interaction、Lifecycle、Transcript 和 Usage 子 reducer。各模块通过 `SessionActionOf` 提取不同的局部子集，但都把别名叫 `Action`，调用签名脱离文件上下文后无法看出该 reducer 的职责。

## 决定

五个子集别名按消费者命名为 `AgentReducerAction`、`InteractionReducerAction`、`LifecycleReducerAction`、`TranscriptReducerAction` 和 `UsageReducerAction`。共享 action union、标签集合和 reducer 组合方式保持不变。

## 替代方案

- 保留每个模块内的 `Action`：拒绝，虽然 lexical scope 不冲突，但这次审计的目标是让 reducer 的动作职责从签名即可定位。
- 为每个 reducer 复制一份 action union：拒绝，`SessionAction` 已经是唯一动作 owner，子集应从该 union 推导。

## 影响与验证

- 仅重命名五个模块私有 TypeScript 类型别名与其 reducer 参数引用。
- reducer 选择的 action 标签、状态更新和运行行为不变。
- 验证：UI `check`、`test:run`、`build`、ADR 索引与差异空白检查。

## 回滚

将五个子集别名恢复为局部 `Action`，并撤回该 reducer 子集命名约定。
