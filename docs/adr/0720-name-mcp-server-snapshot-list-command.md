# ADR 0720：MCP 服务器快照列表命令标明实体

## 状态

已采纳并实施。它收敛 MCP 服务器列表命令的实体名；其它只读集合命令仍按各自返回实体命名。

## 背景

Tauri 命令 `list_mcp_tools` 与 UI wrapper `listMcpTools` 返回 `Vec<McpServerSnapshot>`，每项含服务器配置、连接状态和该服务器发现的工具清单。ToolsView 将结果存入 `mcpServers` 并用于服务器列表、配置编辑与连接状态展示。`tools` 只描述快照的一个字段，不能说明命令返回的实体。

这与已移除的泛化 `get_tools` 属于同一命名错误：集合动词正确，实体名没有对齐真实返回对象。当前 TypeScript generated command contract、命令安全目录、UI 调用 helper 和 IPC 文档都沿用了旧称。

## 决定

- Rust handler、Tauri command、安全目录和 generated contract 统一命名为 `list_mcp_servers`；UI wrapper、ToolsView 与测试统一命名为 `listMcpServers`。
- 继续返回 `Vec<McpServerSnapshot>`。快照仍含配置、连接状态与工具清单；服务器排序、configured-but-disabled server 补全、env 值脱敏、MCP 工具调用授权和状态校验均不改变。
- 更新 IPC contract 文档、输出清单、架构路线图、命名规范和 IPC drift/owner checks。ADR 0549 的历史状态链接到本记录。
- 删除旧 command/function 名称，不添加 Serde alias、Tauri command alias 或 UI wrapper alias。旧名只可在历史 ADR 和说明该错误的审计记录中出现。

## 替代方案

- 只改 UI wrapper 而保留 wire command：拒绝。两层都会继续把服务器快照误称为工具集合，generated contract 也保留错误实体名。
- 保留 `list_mcp_tools`，因为返回项里包含 tools：拒绝。消费者以服务器为行实体，工具只是其嵌套清单。
- 将结果 DTO 改成另一个名字或拆出单独工具列表：拒绝。`McpServerSnapshot` 已准确表达单项语义，拆分会改变设置 UI 所需的配置/状态投影。

## 影响与验证

这是破坏性的 Tauri command 名称与 UI wrapper 名称变更；命令参数和返回 JSON shape、持久化数据及运行时行为不变。版本化配置和数据库无需迁移或重置，调用方随应用一起更新。环境变量仍只以 `<redacted>` 值离开后端。

验证：`scripts/generate-ipc-contracts.ps1` 更新生成命令目录；`scripts/check-ipc-contracts.ps1` 核对 handler、generated command、安全目录、wrapper owner 与文档名称；另运行 IPC Rust/UI 切片要求的 workspace 编译、Clippy、测试、UI check/test/build、IPC contract/event checks、ADR index 与 diff 检查。

## 回滚

若回滚，Rust handler、Tauri 注册、安全目录、generated contract、UI wrapper/消费者和对应文档必须一并还原为旧名。无需恢复或重置持久数据。
