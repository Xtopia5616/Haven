# ADR 0677：删除未使用的消息 thread_id 保留字段

## 背景

Messaging `Envelope` 暴露可选 `thread_id`，注释说明它用于多轮交流分组，但当前工具不读取它，字段只为互操作而保留。全仓唯一引用是默认构造赋值和验证该字段 JSON 往返的测试；消息回复关系已由 `in_reply_to` 承载。

未实现的 `thread_id` 既没有消费者，也没有当前产品语义 owner。它让 `Envelope` 看似支持独立 thread 分组，却无法在消息查询、工具参数或 UI 中使用。

## 决定

- 从 `Envelope` 删除 `thread_id` 字段及默认构造赋值。
- 删除只验证该字段存取的往返测试。
- 保留 `in_reply_to` 作为显式消息回复关联；不新增线程分组模型。
- 不保留旧字段 alias。新的 JSONL envelope 不再输出 `thread_id`；此 crate 不为该未实现字段增加兼容迁移。

## 考虑过的方案

- 保留为未来扩展点：没有活跃消费者或具体分组契约，未来需求应在定义完整 owner 和生命周期后再加入。
- 将它映射到 Session：peer-agent inbox thread 与 Haven 用户 Session 没有现成身份或生命周期对应关系，不能借用 `session_id` 伪装。

## 验证

- 全仓 `thread_id` 搜索确认 Messaging envelope 代码引用已清除；Windows 进程 thread ID 与 logging thread ID 属于独立操作系统概念。
- ADR 索引检查与 `git diff --check`
- 未运行测试；本轮只执行源码与文档审查。

## 回滚与重置

若未来实现 message thread 分组，应以具体消息分组语义重新设计 envelope 字段与消费者，不恢复此未使用字段作为兼容入口。持久 inbox 数据无需重置。
