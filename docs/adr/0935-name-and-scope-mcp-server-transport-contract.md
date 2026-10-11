# 0935：统一 MCP server transport 命名与契约 owner

## 状态

已接受并实现（2026-10-11）。

## 背景

`McpTransportType` 通过 Common root 与 `types` 两个路径暴露，配置 DTO 还在通用配置模块内部从 `types` 导入。该名字把领域专属 transport 描述成通用 “type”，同时没有说明它是 MCP server 连接方式；这一词汇被 Rust protocol、Tauri IPC 生成类型和 UI 编辑器重复使用。

## 决定

- 将类型改名为 `McpServerTransport`，明确它描述 MCP server 的 stdio/HTTP 连接方式。
- 将定义移入 `haven_common::mcp`，Common root 和通用 `types` 模块都不再导出该类型。
- Common config、Tools 管理操作、MCP client/protocol、App IPC 与 UI 均使用同一类型名/子域；重新生成 TypeScript contract。
- 保持 JSON/TOML/IPC 值 `stdio` 与 `http` 不变，不保留旧类型名或旧 generated alias。

## 影响

这是未发布 workspace API 与 generated TypeScript contract 的破坏性重命名。字段仍叫 `transport`，wire value 不变；Rust、TypeScript 源码类型名与 generated constant 名变更。

## 验证

- 静态搜索确认 Rust/UI 源码与生成契约没有旧 `McpTransportType` 或旧 generated aliases；历史 ADR 保留当时的决策名称。
- IPC contract generator/check 与 UI type check/test。
- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked -- --test-threads=1`
- `cargo clippy --workspace --locked -- -D warnings`
- ADR 索引检查。

## 替代方案

- 仅删除 Common root alias并保留 `types::McpTransportType`：拒绝，仍使用通用类型命名空间与含混名称。
- 把类型放进 `config`：拒绝，MCP client、协议 snapshot 与管理面均消费此连接词汇，它不只属于 TOML 配置 DTO。

## 回滚

若新的 MCP transport 领域 owner 后续变化，应将唯一类型定义和所有调用方整体迁移到一个更合适的共享 MCP 子域；不要恢复旧名或双路径。
