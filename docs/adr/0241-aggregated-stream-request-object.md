# ADR 0241：聚合流请求对象

- 状态：已采纳（2026-09-24）
- 范围：`haven-llm` 聚合 streaming Router 入口与 ReAct 调用点
- 关联：[ADR 0234](0234-llm-complete-request-object.md)

## 背景

聚合 stream 主入口以多个位置参数承载 request kind、消息、工具和 output cap，而 cancel token 与 attempt hooks 是另一类执行控制。
参数组容易在 ReAct/Router 边界漂移，但内部 `StreamContext` 已经正确负责重试共享状态。

## 决定

1. 用借用式 `StreamRequest<'a>` 承载 request、messages、tools 和 max output tokens。
2. cancel token、attempt hooks 继续作为独立生命周期控制参数，不放入 DTO。
3. 只收敛带 hooks 的聚合 stream 主入口；raw `chat_stream`、便利包装、内部 `StreamContext` 和 provider adapter 保持不变。
4. 不改变 permit、health、retry、usage、cancel 优先级或 output cap 传递。

## 影响与验证

普通/带工具聚合流、取消和 output cap 的回归测试通过；llm 434 项、agent 468 项及对应严格 Clippy 通过。

## 回滚

恢复聚合入口的四个位置参数并删除 `StreamRequest`；不涉及 provider wire、schema 或持久数据。
