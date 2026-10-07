# ADR 0727：持久消息类别复用 TranscriptMessageKind

## 状态

已采纳并实施。

## 背景

`messages.message_type` 的 SQLite `CHECK` 已限定六种持久类别；Common、Memory `Message`、Memory 写入 API、Agent history 与 resume IPC 却把它当作自由字符串。`SessionResumeResponse` 直接包含 Memory `Message`，因此 TypeScript generated contract 也无法表达数据库闭合集合。UI resume mapper 还会把持久 `observation` 投影成 renderer 的 `tool` 展示类型；两者是不同概念，不能把 UI discriminator 写回 durable message kind。

## 决定

- Common 定义 `TranscriptMessageKind`，唯一拥有 `text`、`thought`、`tool_call`、`observation`、`reasoning`、`peer_kickoff` 六个持久值。
- Memory message DTO 和写入 API、event projection、Agent 使用点均使用该 enum。SQLite 继续存原有 snake_case 文本；写入从 enum 得到字符串，读取严格解析并拒绝未知值。
- resume IPC 从 Rust enum 生成 TypeScript 联合类型。UI 按 `TranscriptMessageKind` 处理 durable message，映射到 renderer 自己的 `StreamMessage.type`；`observation` 仍显示为工具卡片，持久类别不等同于展示类型。
- UI renderer discriminator 与 provider 消息类型不改动；此 ADR 只收口 Haven durable transcript category。

## 替代方案

- 只收窄 TypeScript 类型：拒绝。Memory 与 Agent 的写入/读取 API 仍允许越过 schema 集合。
- 复用 `StreamMessage.type` 作为持久类别：拒绝。renderer 的 `tool` 是从 `observation` 投影出的展示值，语义不同。
- 将数据库列改为别的编码或另建兼容解析：拒绝。现有字符串和值集合已经稳定，且项目无需向下兼容。

## 影响与验证

Rust 公共字段/API 与 resume TypeScript contract 收窄；JSON 字符串和 SQLite 合法值不变。数据库 schema 与持久化格式未变化，无需重置数据库。新增闭合集合测试与越界数据库值读取回归测试。

验证：Rust workspace fmt/check/Clippy/tests、UI check/tests/build、IPC 生成与漂移检查、事件检查、ADR 索引检查及 diff checks。

## 回滚

恢复自由字符串 API/type 并撤销 resume contract enum 即可；SQLite 行和 JSON 值无需数据回滚。
