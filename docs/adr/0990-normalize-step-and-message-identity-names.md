# ADR 0990：统一步骤计数与消息身份名称

## 状态

已采纳并实施。

## 背景

事件与数据库已使用 `step_number`，但 Agent 的 ReAct context、loop/helper 参数和局部变量仍广泛缩写为 `step_num`。消息身份也在 Agent ingress、工具测试和 UI chat controller 中分别写作 `msg_id` / `msgId`，与 Rust `message_id` 和前端 `messageId` 的实体名不一致。

## 决定

1. Agent 内部步骤计数统一使用 `step_number`，tracing 字段也使用同名；事件、数据库与前端 wire shape 不变。
2. Rust 内部消息引用统一使用 `message_id`，前端内部引用统一使用 `messageId`；实体 ID 值与命令/事件契约不变。
3. 新代码遵循 `docs/naming.md`，不再引入这两类缩写。

## 影响与验证

- Agent ReAct 与工具执行 helper 的 context、参数、局部值和测试 fixture 使用 `step_number`。
- ingress、Messaging 测试与 UI chat controller 使用完整的 message identity 字段名。
- 复扫 Rust/UI 源码无 `step_num`、`msg_id`、`msgId` 遗留；运行 workspace 与 UI 检查。

## 回滚

恢复内部缩写即可；无需数据或 wire contract 迁移。回滚会重新引入跨层同一实体的多种字段名。
