# ADR 0199：删除 sessions.transcript 快照列

日期：2026-09-22
状态：已采纳

## 背景

sessions.transcript 只在创建会话时写入，之后不会随对话持续更新。会话正文实际
由 session_events 的 append-only 事件流承载，并物化到 messages；保留该列会让
历史列表、搜索和 Session DTO 看起来存在第二个 transcript 来源。

## 决定

1. 删除 sessions.transcript 列、Session.transcript 字段和
   Database::create_session 的旧 transcript 参数。
2. session_events 继续是恢复、回滚和 live replay 的 transcript 权威来源；
   messages 继续是 UI 与搜索使用的物化投影；sessions.input_text 仅保留会话初始
   输入/列表摘要用途。
3. 会话搜索不再读取 sessions.transcript，正文匹配通过 messages.content 的
   关联查询完成。历史页使用 input_text，不再消费已删除的快照字段。
4. 数据库 schema 从 v24 提升到 v25。没有运行时迁移；已有数据库按发布说明删除并
   重建。

## 影响与验证

删除了重复持久化和旧的 DTO/SQL 入口，减少了搜索结果与恢复内容分叉的可能。验证
覆盖 sessions 列契约、消息正文搜索、haven-memory 测试、haven-agent 测试及
前端类型检查/测试。
