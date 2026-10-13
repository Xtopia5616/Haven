# ADR 1130：显式命名流式工具调用元数据

## 背景

持久工具调用和 provider 映射已经统一使用 `tool_call_id`，执行顺序使用 `tool_index`。LLM 的 `StreamToolCallUpdate` 仍把这两个值称为 `id` 与 `index`，Agent 消费方因此需要从上下文猜字段含义。Compaction summary prompt 也将 provider 调用关联值打印为泛名 `id`。

## 决定

- `StreamToolCallUpdate.id` / `index` 改为 `tool_call_id` / `tool_index`；所有 provider adapter 和 Agent consumer 直接使用规范名称。
- Summary prompt 将结果关联标记写作 `tool_call_id`，明确它是 provider 调用关联身份，不是消息或结果实体 ID。
- Tools 内表示 ToolRun 实体身份的局部变量和测试参数使用 `tool_run_id`；provider 的外部 ID 值保持原样。

## 影响与验证

这是 Rust crate 内部 API 与 prompt 标签的命名收敛，不改变 provider wire payload、durable event、IPC 或数据库字段，也不需要数据迁移或重置。由 LLM adapter、Agent stream 和 compaction 测试覆盖；Tools 切片另运行 `cargo test --locked -p haven-tools`。

## 回滚

可将内部 Rust 字段恢复为 `id` / `index` 并恢复旧 prompt 标签，不涉及持久数据或 provider 协议。
