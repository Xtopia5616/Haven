# ADR 1139：显式命名 Memory 与事件 API 的实体身份

## 背景

全库命名复核发现，若干 Memory Repository 和事件 API 仍以 `id` 或 `ids` 表达具体持久实体身份。部分身份在同一行存在两个合法命名空间，例如 thought step 可复用 summary/message 的 `msg-*` ID；仅按函数上下文使用 `id` 会掩盖其实际关联。

## 决定

- Message、SessionStep、episode summary、Fact、Node、Session 和 EventBus subscriber 的参数与局部身份变量使用对应实体名；Fact ID 列表使用 `fact_ids`。
- 可接受 `step-*` 或共享 `msg-*` 身份的 SessionStep 行在读取校验时命名为 `step_or_thought_message_id`。
- 保留真正泛型的 canonical-ID 行读取 helper、通用 ID validator、provider/wire ID 与测试局部 helper 的 `id` 名称。
- 后续 Rust API 参数若承载多个实体 ID 前缀，或实体共享身份空间，必须按实际身份关系命名，不仅按函数名推断类型。

## 影响与验证

这些变更只规范 Rust 参数和局部变量名，不改变 SQLite 字段、ID 前缀、序列化格式、运行时行为或调用顺序。按命名文档执行 Rust 格式检查、workspace 测试与严格 Clippy。

## 回滚

可恢复旧的 Rust 源码标识符；不得借此改变持久列名或放宽共享身份校验。
