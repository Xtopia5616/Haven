# ADR 0762：Agent usage renderer 复用闭合 accounting 类型

## 状态

已采纳并实施。

## 背景

Agent live usage producer 的 `role` 是 `Option<RequestKind>`，cache accounting 的运行时 owner 是
`CacheAccounting::{inclusive, exclusive, unknown}`。UI mapper 原先只检查 `role` 和
`cache_accounting` 是字符串，随后把任意值放进 renderer payload。这样动态事件可以越过 Rust
enum 的值域约束。

ADR 0270 明确保留 IPC JSON usage payload 的字符串表示；durable/resume usage projection 也继续
以字符串读取，以兼容已有持久记录。因此，本切片只收紧 live Agent renderer 边界。

## 决定

- IPC generator 显式导出 Common `CacheAccounting` union/value list；`RequestKind` 继续复用既有生成 owner。
- `AgentUsagePayload.cacheAccounting` 与可选 `role` 使用生成类型；runtime mapper 对两字段校验闭合值域，拒绝未知值。
- 不改变 Rust producer DTO、JSON key/value 表示、历史 usage 字符串或数据库 schema。resume/durable usage 的开放字符串 owner 仍按 ADR 0270 保留。

## 影响与验证

UI live usage view 不再接受任意 accounting/request-role 字符串；生成契约新增 `CacheAccounting` 类型和值列表。
Rust/IPC wire 与持久数据不变。IPC drift check、Rust workspace tests、严格 Clippy、Rust fmt，以及固定
Node 24.20.0 下的 UI check/test/build 均通过。

## 回滚

从生成器移除 `CacheAccounting` 导出，并恢复 UI renderer view 与 mapper 的开放字符串检查即可；不涉及
IPC 字面量、数据库或持久化迁移。
