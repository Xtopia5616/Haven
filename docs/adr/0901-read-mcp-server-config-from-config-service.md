# 0901：让 MCP profile 只由 ConfigService 持有

## 状态

已接受并实现（2026-10-10）。

## 背景

`ConfigService` 是进程内的权威配置 owner，但 Tools 又维护 `mcp_server_configs: HashMap` 副本。启动加载、发现、Admin 更新/删除/reload/refresh 以及 App bridge 命令都需要手动同步它。`LoadMcpTool`、`ToolCatalogTool`、`AdminServices` 和 App MCP 状态命令各自读取这份副本；Admin 状态甚至在副本为空时改读 `ConfigLoader`。这形成多条来源和 fallback 语义，更新漏同步时会让已保存 profile、工具加载和 UI 状态彼此不一致。

MCP 活动连接、client 启动时的 profile 和发现结果属于 `McpManager` 的运行时状态，仍由它持有；这些值描述当前连接，不再作为读取 server 配置的来源。

## 决定

- MCP server profile 只从 `ConfigService::snapshot()` 读取。Tools 内部 `McpServerConfigSource` 绑定同一个配置 service 并返回 typed profile 快照；不保留生产 `HashMap` 镜像。
- `LoadMcpTool`、`ToolCatalogTool`、`AdminServices` 和 app 的 MCP 状态投影都使用当前配置快照。配置读取失败时，loader/catalog/Admin/App 路径返回错误；能力索引记录错误并按不可用处理，不把故障伪装成正常空配置。
- `load_mcp_from_config` 与 discovery 仍把本次应用的配置传给 `McpManager`，但不再把同一配置另存进 Tools。McpManager 继续拥有活动连接和发现缓存。
- 删除 `ToolsFacade` 上用于同步 profile map 的生产 upsert/remove/list 操作。跨 crate 测试仅通过非默认 `ToolCatalogTestSupportPort` 注入配置 fixture；Tools 单元测试使用 crate 内部 `cfg(test)` fixture。
- `ConfigService` 由 App 在 `wire_startup` 绑定到 Tools 内部 source。没有配置 service 的 headless Tools 实例没有 MCP profile；这不改变 App 的生产启动路径。
- 通用规则补充到 `docs/development-standards.md`：权威配置不可由消费者维护需手工同步的可变影子副本。

## 影响

- 只有一个 MCP profile 配置来源；禁用的 server 仍包含在状态投影，且环境变量仍由现有 App IPC redaction 处理。
- MCP 连接、重连、refresh、按需加载、工具目录与 prompt 能力索引行为保持原定职责；运行时 client 的配置比较不被移除。
- 数据库、Serde、Tauri IPC shape 与配置格式均不变。默认 Rust API 有意破坏性收口，不保留旧 map 同步方法。
- MCP 配置 source 的 test fixtures 只在单测或显式 `test-support` feature 编译。

## 验证

以下门禁在固定 Windows 工具链通过：

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --locked -p haven-tools`（805 passed、2 ignored；MCP 集成 7 passed）
- `cargo test --workspace --locked`（全 workspace 通过；Agent 619 passed、App 235 passed、Memory 408 passed、Tools 805 passed、MCP 集成 7 passed；包含既有 ignored tests）

## 替代方案

- 保留 map 并要求每个 writer 更新：拒绝，因为该规则无法由类型或单一 owner 保证，当前已有多处同步和空 map fallback。
- 让 `McpManager` 继续作为 profile owner：拒绝，因为 server profile 即使没有连接也必须能查询和编辑；`McpManager` 只拥有活动 client 与发现状态。
- 将配置复制到一个新的 Tools-owned cache service：拒绝，因为这只包装了第二份可变权威状态；配置 snapshot 应直接来自 `ConfigService`。
