# ADR 1342：为压缩后的会话提供原始历史检索工具

## 背景

上下文压缩会从当前模型请求中移除较早的逐条对话和工具细节，但 `session_events` 仍保留 transcript 原始事件。压缩后的摘要不一定保留精确的用户输入、工具参数或工具结果；模型需要一个受限入口，在后续轮次按需找回这些记录。

## 决定

- 在 `session` 工具增加 `history_search` 与 `history_read`，仅允许通过 Tools 注入的当前 session ID 访问本会话。
- 搜索原始用户输入、助手内容、ToolCall、ToolResult 和媒体计划 payload，按匹配位置返回最新的最多 5 条记录；思考/推理事件和 ToolCall 内嵌的 `reasoning` / `thinking_blocks` 字段不暴露。可用 `before_sequence` 向更早结果翻页。通过返回的 sequence 可读取单条记录，响应会报告被移除的字段。
- 检索包含压缩前的 transcript 原始事件，但跳过 `compact_summary`；回滚标记会从当前 lineage 中移除其目标 sequence 之后的记录。搜索和精确读取均使用同一个有界扫描与分支过滤规则。
- 单次扫描最多处理 50,000 条 session event 或 64 MiB payload。超过任一上限时显式失败，不返回无法确认仍属于当前 lineage 的部分结果。
- 搜索每条记录最多返回 2 KiB 邻近文本；精确读取最多返回 32 KiB payload，并报告原始 payload 字节数、已移除字段和是否截断。读取仅用于补回被省略的原始细节，调用方应继续使用最近对话与 compaction summary 作为主上下文。
- transcript 内容是低信任数据；工具说明和结果标注不得将历史文本中的指令当成当前指令。
- 搜索能力由 Memory 持有分支回放与扫描边界；Tools 只将其作为只读、幂等的 session 操作暴露给模型。

## 替代方案

- 仅依赖 compaction summary：不能可靠恢复精确工具参数或长工具输出，拒绝。
- 使用跨会话语义记忆搜索：适合找持久事实，不保证返回当前会话的原始记录，拒绝。
- 直接将完整 transcript 全量重新注入 prompt：会抵消压缩的 token 节省，且缺少查询范围与输出边界，拒绝。

## 影响

新增两个模型可见的只读工具操作，以及 Memory 的有界 transcript 查询能力。没有新增持久数据格式、表或实体 ID，不需要数据库迁移或用户数据重置。历史记录按当前 session 的回滚 lineage 过滤，避免模型重新读取已回滚的工具输入/结果。

## 验证

- Memory 单元测试覆盖压缩摘要排除、结果翻页、回滚分支过滤、内部推理字段移除、精确序列读取、取消、扫描上限和 UTF-8 安全截断。
- Tools 单元测试覆盖操作注册、当前 session 注入和 search/read 调用链。
- 运行 `cargo fmt --all -- --check`、`cargo test --locked -p haven-memory event_history`、`cargo test --locked -p haven-tools session`、`cargo clippy --locked -p haven-memory -p haven-tools -- -D warnings`。

## 回滚 / 重置

回滚代码提交即可移除模型可见操作；数据库 schema 与已有 session events 不变，无需迁移或重置数据。
