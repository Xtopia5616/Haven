# ADR 0761：Agent media-plan event 复用生成闭合 enum

## 状态

已采纳并实施。

## 背景

Rust `AgentMediaPlanEvent` 的 `role`、`MediaInputStrategy`、`MediaRepresentationKind`、`MediaProjectionMode` 与 `MediaPlanNoticeCode` 都是闭合枚举。后两种 Common enum 尚未进入 UI 生成契约；Agent UI mapper 曾把全部这些字段当开放字符串，连 ADR 0113/0116 标为稳定的 notice code 也接受未来未知成员。该接受策略与 UI 单一 Rust binary 的部署方式及 `docs/naming.md` 的闭合契约规则不一致。

## 决定

- IPC 生成器显式输出 Common `MediaProjectionMode` 与 `MediaPlanNoticeCode`，连同其 TypeScript union 和 values 清单；已有的 `RequestKind`、`MediaInputStrategy` 与 `MediaRepresentationKind` 继续复用既有生成 owner。
- `AgentMediaPlanPayload` 的对应字段引用生成 enum；`mapAgentEvent` 在根字段和 projection/notice 每个数组项上校验值域，未知闭合成员使整个事件映射失败。
- 未知附加字段仍被忽略；未消费的 `provenance` 不加入 renderer view，media-plan JSON 字段名和值不变。

## 替代方案

保留 `string` 会让稳定的 Common enum 与 UI 类型/校验漂移。手写值列表会在前端复制 Rust owner；把整个 Agent event DTO 纳入 generated wire 契约则会扩大本切片范围，而当前只需投影字段及三个已有 enum 的值域。

## 影响与验证

新增生成 enum/value list，并收紧 Agent media-plan event 的 renderer DTO 与 runtime mapper；不改变 producer、JSON、事件顺序、IPC command 或持久化。未知闭合值现在按畸形事件丢弃，未知 additive fields 仍兼容。Rust workspace tests、严格 Clippy、IPC drift check、固定 Node 24.20.0 下的 UI check/test/build 均通过。

## 回滚

移除生成器中的两个 Common enum 导出并恢复 mapper 的字符串类型/检查即可；没有 IPC 字面量、数据库或持久化迁移。
