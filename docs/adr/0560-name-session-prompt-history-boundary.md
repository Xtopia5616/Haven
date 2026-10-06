# ADR 0560：明确 Memory prompt history 查询与 Agent 输入的命名边界

## 状态

已采纳并实施；Rust workspace 门禁通过。

## 背景

Memory `SessionStore::conversation_window` 按 session 查询最近的纯文本消息，返回 `SessionMessageText { id, role, content }`；Agent 随后把它转换成同字段的私有 `ConversationMessage`，供 fresh-run 的首次 session prompt 使用。调用边界真正区分的是 Memory 读取出的消息文本行与 Agent 所有的 prompt 输入，但名字用 `conversation_window`、`conversation_window_size`、`conv_history` 和泛称 `ConversationMessage`，不能一眼看出该用途。上层配置键 `memory.session_window_size` 已持久化。

## 决定

1. Memory 查询改为 `list_session_prompt_messages`，保持其 bounded latest-message 查询、顺序、筛选与 limit 语义不变。
2. Agent 私有输入类型改为 `SessionPromptMessage`，loader 改为 `load_session_prompt_history`；limit、局部 history 与 `run_session` 参数统一采用 `session_prompt_history_limit` / `session_prompt_history`。
3. Memory 的 `SessionMessageText` 与 Agent 的 `SessionPromptMessage` 保持不同 owner：Memory 类型表达存储查询读模型，Agent 类型表达仅供 prompt 组装的私有输入；Memory 不依赖 Agent，prompt 专属类型也不成为 Memory API 的规范类型。
4. 配置 key `memory.session_window_size` 不改，数据库字段、IPC、事件、prompt 内容、S1 去重身份规则与新会话启动行为均不变。其他位置的 `conversation_history` 在表达自然语言历史文本时可继续使用。

## 替代方案

- 让 Agent 直接持有 `SessionMessageText` 并删除其私有 prompt DTO：拒绝。这样会让 Agent prompt 组装结构由 Memory 读模型定义，丢失现有清晰的 crate ownership 边界。
- 把所有 `conversation` 名称替换为 `session`：拒绝。自然语言历史和模型上下文是 conversation；这次只统一实际指向 session prompt 消息输入的 API 与局部值。
- 重命名持久配置 key：拒绝。它会改变配置契约且此切片不需要变更持久格式。

## 影响与验证

- 这是 Agent/Memory 内部 Rust API 与局部类型命名变更；无 schema、配置格式、wire、事件或运行行为变化。
- 验证 Rust workspace fmt、locked check、strict Clippy 与全 workspace tests；`APPDATA` 使用 ADR 独立目录隔离。

## 回滚

恢复原方法、私有类型与局部字段名并同步更新测试；无需数据库或配置迁移/重置。
