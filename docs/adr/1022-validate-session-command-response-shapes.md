# 1022：从 Rust 契约校验完整 Session 响应

## 状态

已采纳并实施（2026-10-12）。

## 背景

`list_runtime_sessions`、历史/lineage 查询和 resume 命令此前只在部分路径校验 Session、Step、Message、用量、交互和媒体资产 ID。缺字段、错误类型或未知枚举仍可能被断言为生成 DTO 并进入 UI。生成契约描述了 Rust 字段，但没有覆盖这些命令的完整运行时响应。

## 决定

- IPC generator 为 runtime Session 列表、SessionRecord、lineage 和 resume 响应生成递归 Serde wire shape。生成器支持外部、内部、相邻与 untagged enum、字面判别值、null 和 tuple，供结构化媒体与 owner 枚举复用。
- `sessionHistory.ts` 在原有身份与关联检查后，校验完整 generated shape。对象允许附加字段以保持扩展兼容；所有必需字段、显式 nullable、已提供 optional 字段、容器和闭合枚举按 Rust wire 定义检查。
- Session 边界继续校验领域身份与关系：session/message/step/usage/asset ID、resume 所有者、行间 session 关联、Ask 与步骤关联、消息与步骤关联、lineage 锚点和受管媒体引用。
- 数值校验匹配 Rust 原始类型：`i32` 使用有符号 32 位范围，`u32` 使用非负 32 位范围，JSON `u64` 使用 JavaScript 安全非负整数范围，浮点值必须有限。runtime `waiting_reason` 仅能与 `paused` 状态共存。
- `check-ipc-contracts.ps1` 锁定四个生成 shape、parser 调用以及整数和身份 guard 的存在。

## 影响

不改变 Rust DTO、IPC JSON、持久化数据或 UI 语义。非法或不完整响应会在进入 renderer state 前失败；合法附加字段仍保留。生成的 wire shape 更大，因为 resume 包含递归媒体 DTO；这是 Rust 结构化 enum 与 payload 完整校验的直接结果。

## 替代方案

- 仅检查实体 ID：拒绝，无法阻止缺字段或枚举漂移进入 renderer。
- 手写 TypeScript shape：拒绝，造成与 Rust Serde DTO 重复维护。
- 对所有字段拒绝未知附加属性：拒绝，这会无关地收紧 additive compatibility。

## 验证

覆盖 runtime/history/lineage/resume 完整行、必需字段、闭合枚举、身份关联、媒体嵌套引用、整数边界和等待状态语义。适用门禁包含 Rust workspace tests/check/clippy、UI check/tests/build、IPC code generation/check、IPC ownership gate、ADR index 和 diff checks。

## 回滚

回退 generated shapes 与 Session parser 检查会恢复仅检查部分 identity 的旧行为。无数据库、配置或 wire format 迁移。
