# ADR 1330：收敛 ReAct turn 准备、安全准入与边界 owner

## 状态

已采纳并实施。

## 背景

ReAct 的 `LoopHooks` 同时承载 turn 准备、响应分类、工具授权和暂停记忆触发。工具准入 trait 默认返回 `Proceed`；响应 hook 只是纯 classifier 的转发；暂停 hook 只生成 payload，而 event boundary 才负责核验和追加。准备结果将工具目录、定义和 token 估算拆成可独立缺省字段，允许请求准备收到半成品。

## 决定

1. 工具调用由必需实现的 `ToolAdmission::admit_tool` 做准入。没有默认 `Proceed`；生产实现继续检查持久确认决定和当前授权策略。批次保留 `step_id`、`tool_index`、`tool_call_id` 校验，并且必须先收齐全部准入结果，再启动任何工具执行。
2. Turn 输入注入后由 `TurnPreparation::prepare` 完成准备并返回 `PreparedTurn`。其中 catalog、provider 定义和 token 估算是完整且不可缺省的工具面；区间记忆触发仍为可选。准备保留 MEMORY patch → 工具目录/定义解析 → 压缩 → 请求投影/路由准备 → 取消与 deadline 检查 → 单次 bounded steering drain 的顺序。Late steering 重建继续复用冻结的工具定义和 token 估算。
3. 删除 `after_llm` hook，由 `response_cycle` 直接调用 `ResponsePolicy::classify`。分类决策仍由 response cycle 执行重试或终止响应；分类与策略执行保持两个阶段，符合 [ADR 0637](0637-unify-react-response-policy-types.md)。
4. 暂停记忆触发的构造与追加归 `event_boundary`。各暂停路径仍先验证 durable event boundary、更新生命周期状态，再以对应原因追加触发。删除泛化 `on_pause` hook。

## 影响与验证

这是 Agent 内部 ReAct 协作接口与安全默认值的变化，不改变 transcript、事件 payload、数据库、配置或 IPC 契约，无需数据重置。未显式实现安全准入会在编译时失败；测试用的拒绝型准入明确 fail closed。

验证覆盖响应准备期间 steering 输入的原有顺序与单次补读、暂停原因到 payload 的映射、tool batch 全部准入后才执行，以及 Rust agent crate 的 locked 编译、测试与严格 Clippy。

## 回滚

恢复泛化 hook 前，必须保留工具准入的显式实现要求和批次屏障。此 ADR 不需要数据或 wire 回滚。
