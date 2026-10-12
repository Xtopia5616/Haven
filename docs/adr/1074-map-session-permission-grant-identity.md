# ADR 1074：将 Session permission grant 身份映射到 renderer DTO

## 状态

已接受（2026-10-13）

## 背景

`list_session_permissions` 的 generated Rust DTO 使用 `session_id`。Settings contract 已校验它是 canonical `ses-*`，但把 generated DTO 原样返回给 renderer，页面继续读取 snake_case 实体身份，违反 UI 边界使用 camelCase `xxxId` 的约定。

## 决定

- 保留 wire parser 对生成 shape、capability scope 和 canonical session identity 的校验。
- 校验后只将 `session_id` 投影为 renderer DTO 的 `sessionId`；其它字段保持当前 wire 名称和值，不增加第二套无关字段映射。
- Settings command adapter 与组件只使用该 renderer DTO；发起撤销命令时由 command adapter 将 `sessionId` 映射回 Rust wire 入参。

## 替代方案

- 继续在 renderer 使用 `session_id`：不采纳，因为 Rust/IPC snake_case 应止于边界。
- 重命名所有 grant 属性：不采纳，因为当前漂移只涉及实体身份，扩大映射会造成无益的 DTO 重复。

## 影响

Renderer 内部类型发生字段名变化；Tauri command wire shape、授权持久语义和撤销行为不变。无数据库迁移或重置要求。

## 验证

- Settings parser 测试验证 `session_id` 到 `sessionId` 的映射及 snake_case 字段不泄漏。
- Settings command 与组件测试使用 renderer DTO。
- UI 类型检查、全量 UI 测试和 IPC contract 检查通过。

## 回滚与数据重置

无持久化或 wire 变更，不需要重置。若回滚，应同时恢复 parser 返回类型、Settings 消费者和对应测试；不得跳过已有的 wire shape 与 identity 校验。
