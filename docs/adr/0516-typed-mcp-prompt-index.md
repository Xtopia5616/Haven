# ADR 0516：用 typed projection 表达 MCP prompt index

## 状态

已采纳并实现（2026-10-05）。

## 背景与证据

`ToolsManager::build_mcp_index` 生成固定字段 `name`、`tool_names`、`description`、`tool_count`，但以 `Vec<serde_json::Value>` 跨 Tools、App adapter 和 Agent prompt port 传递。`description` 是由工具名拼接的 `"; tools:"` 字符串，`tool_count` 又重复保存 `tool_names.len()`。

当前存在两条生产消费路径：

1. Tools 的 runtime capability resolver 忽略 `tool_names`，从 `description` 再拆分分隔符和逗号来判断是否有搜索工具；
2. Agent prompt renderer 按 JSON key 读取 `name`、`tool_names` 和 `tool_count`，对缺失或错型字段静默回退为空值或数组长度。

这使固定的跨 crate 内部摘要缺少类型契约，并让 capability 判断依赖 renderer 描述文案的格式。`list_schemas_for_session` 的 `Value` 则表示动态 JSON Schema，属于不同且应保留的扩展边界。

## 决定

1. `haven-tools` 拥有公开的 `McpServerIndexEntry { name, tool_names }` 类型；`ToolsManager::build_mcp_index` 返回 `Vec<McpServerIndexEntry>`。该类型只承载两项底层事实，不实现 `Serialize`，也不进入 MCP/provider/IPC wire。
2. 保留当前 enabled-only 过滤、名称 sanitizer、cached tool name 来源、排序和去重。删除由这些字段派生的 `description` 与 `tool_count`，避免双重表示；工具数量一律由 `tool_names.len()` 得出。
3. runtime capability resolver 直接从 typed `tool_names` 派生 MCP search availability，不解析 prompt 文案；维持当前 provider search 优先于 MCP search、再到 unavailable 的优先顺序及工具名中包含 `search` 的识别规则。
4. Agent renderer 直接读取 typed `name` 和 `tool_names`，并从名称列表派生数量。会话 prompt 的数量显示、最多显示 8 个工具名以及超出时提示 `load_mcp` 的文案保持不变；不再生成或传递派生描述字段。
5. 仅收窄固定 prompt index。MCP tool schema、JSON arguments、`ToolResult.output`、`list_schemas_for_session` 等异构/远端 JSON 边界保持动态 `Value`。
6. 所有 workspace 调用点迁移到新返回类型，不留旧 `Vec<Value>` 兼容入口。该签名是公开 Rust source API 变化；Haven 当前为 0.1 测试版本，已审计仓库内消费者为 Tools runtime/tests、App agent adapter 与 Agent prompt port/tests。

## 行为与影响

不改 prompt 内容、tool loading、search capability 优先级、MCP 协议、模型可见 tool schema、Tauri/TypeScript IPC、持久化、配置或用户数据。无需数据库/配置重置，也不改变 crate 依赖方向；Agent 和 App 已依赖 `haven-tools`。

替代方案是继续使用 `Value` 并依靠字符串约定，或把类型放入 Common。前者保留重复字段和描述反解析；后者会把 Tools 领域摘要放入过于通用的基础 crate，因此都不采用。

## 验收

- typed projection 测试固定 enabled-only、sanitize、sort/dedup、cached names 和不泄露 MCP process args；
- Tools capability 回归直接基于 `tool_names`，并固定 Provider > MCP > Unavailable；
- Agent prompt renderer 测试固定工具计数、8 项展示阈值及 `load_mcp` 提示；
- 生产链不再解析 `description` 或 `tool_count`，动态 schema 测试与接口保持不变；
- 适用门禁：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`scripts/check-crate-dependencies.ps1`、ADR index 与 `git diff --check`。不涉及 UI/IPC，不运行其门禁。

2026-10-05 完成记录：`cargo test --locked -p haven-tools`（798 passed, 2 ignored；integration 7 passed）、`cargo test --locked -p haven-agent`（591 passed, 1 ignored）、`cargo check --locked -p haven-app-binary`、`cargo check --workspace --locked`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo fmt --all -- --check`、crate dependency check、ADR index check 与 `git diff --check` 均通过。

## 回滚

恢复 `build_mcp_index -> Vec<Value>` 和原有消费者即可；不涉及持久数据或配置回滚。回滚会重新引入通过 `description` 文本解析 capability 与 JSON key 静默回退的内部契约。
