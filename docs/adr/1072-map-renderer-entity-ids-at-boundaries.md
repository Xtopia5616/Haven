# ADR 1072：在 Memory 与会话历史边界映射实体 ID

## 状态

已接受（2026-10-13）

## 背景

Rust/SQLite/IPC 使用 `xxx_id` snake_case，前端实体身份约定使用 `xxxId`。Memory 和 SessionHistory renderer 曾直接把生成的 Rust DTO 暴露给组件，导致相同的业务身份字段在 UI 代码中继续使用 snake_case。

## 决定

- Memory 与 SessionHistory command adapter 先按生成的 Rust wire shape 和 canonical ID 规则校验，再投影到 renderer DTO；fact/source message/entity/session/parent session/step/message/usage/media asset identity 使用 camelCase `xxxId`。
- 非 ID 字段保持当前 wire 形状，不做全对象重命名。
- provider 外部 `tool_call_id` 保留 provider 原始字段名和值，不转换为 Haven ID。
- 组件和测试只消费 renderer DTO，不以内容值相等或 snake_case alias 作为身份回退。

## 影响

内部 UI DTO 形状有意改变；Rust command/event wire contract 与生成器输入不变，无数据库重置要求。

## 验证

- DTO mapper 测试覆盖嵌套 source、lineage、usage 和媒体 asset ID 的投影与校验。
- 全量 UI 类型检查、测试、IPC contract 与 event drift 检查通过。
