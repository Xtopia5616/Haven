# ADR 0726：消息角色复用 CanonicalRole

## 状态

已采纳并实施。

## 背景

`messages.role` 的 SQLite CHECK 只允许 `system`、`user`、`assistant`、`tool`，Common 已有同值域的 `CanonicalRole`，Agent 的 provider-neutral canonical messages 也已使用它。持久 Memory `Message.role`、消息写入 API、session history 投影、Agent resume/fact inference、以及 live/resume UI DTO 却再次使用自由字符串。resume command 直接序列化 Memory `Message`，让前端的 `role` 继续保持开放 `string`；ChatBubble 与 context-menu request 又复制了一层宽类型。

这些边界描述的是同一条 transcript message role。Provider 自身的 wire role 由 LLM adapter 映射；模型发现命令的 `role` 则指模型 ID 或 `RequestKind`，含义不同，不纳入本次收敛。

## 决定

- Common `CanonicalRole` 是 canonical message role 的唯一词汇 owner。Memory 持久 `Message`、插入 API、`SessionMessageText` 与 Agent history/filter 复用该 enum。
- SQLite 保留相同 snake_case 文本。写入从 `CanonicalRole::as_str()` 生成；读取严格解析，CHECK 被绕过或数据损坏时返回 conversion error。
- Rust→TypeScript 生成契约导出 `CanonicalRole`；live/resume message、ChatBubble 与 context-menu role 引用该闭合类型。
- provider-specific message DTO 保持 adapter/wire owner，不为了共享内部词汇改变供应商请求 JSON。
- SQLite schema、合法行、SessionResume JSON 值和消息行为不变；不新增旧字符串 API、IPC alias 或兼容解析。

## 替代方案

- 只把 UI 类型改为手写 union：拒绝。Memory 写入/读取和 Agent history 仍各自接受开放字符串。
- 为持久消息新建 enum：拒绝。它会复制已用于 canonical transcript 的 `CanonicalRole` 值域。
- 用字符串强转绕开读取错误：拒绝。数据库 CHECK 是防误写约束；读边界仍须拒绝越界值。

## 影响与验证

workspace Rust API 与生成 IPC TypeScript 类型收窄；JSON 角色文本继续为原有四个值。未知/大小写不匹配角色不再透传。没有 schema 或持久化格式变化，无需重置数据库。

验证：CanonicalRole 精确解析测试、Memory 损坏 role 读回归测试、Rust workspace fmt/check/Clippy/tests、UI check/tests/build、IPC 生成与漂移检查、事件检查、ADR 索引与 diff checks。

## 回滚

恢复 Memory 和 Agent 的 String 字段及写入参数，并恢复 UI 的开放字符串 role 类型即可；现有 SQLite 文本和 JSON 字符串无需数据回滚。
