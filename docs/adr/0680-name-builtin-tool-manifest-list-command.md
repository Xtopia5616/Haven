# ADR 0680：明确内置工具 manifest 列表命令

## 状态

已采纳并实施。

## 背景

`get_tools` 返回的是内置工具 manifest 列表，包含已启用与已禁用工具的状态；它不包含 Skill 或 MCP 清单。UI wrapper 已经叫 `listTools`，但 Tauri command 仍使用 `get_tools`，响应类型 `ToolListResponse` 也没有说明数据来源。Rust handler 放在 `commands/skills.rs`，与同文件的 `set_tool_enabled`、`reset_tool_circuits` 一起把 Tool 管理命令和 Skill 生命周期混在一个模块。

## 决定

1. Tauri command 与 Rust handler 统一命名为 `list_builtin_tool_manifests`，UI wrapper 命名为 `listBuiltinToolManifests`。
2. 响应类型由 `ToolListResponse` 改为 `BuiltinToolManifestListResponse`；wire 字段仍为 `tools`，其中元素继续使用生成的 `ToolManifest`。
3. `list_builtin_tool_manifests`、`set_tool_enabled` 和 `reset_tool_circuits` 移入 `commands/tools.rs`；`commands/skills.rs` 只保留 Skill 清单、刷新、开关、目录与执行命令。
4. 更新生成契约、command security registry、IPC 文档、输出清单、UI command-owner 检查和路线图。删除旧 command/type/module 入口，不加兼容别名。此决定替代 [ADR 0549](0549-use-list-for-ui-collection-queries.md) 中保留 `get_tools` 的部分。

## 替代方案

- 使用 `list_tools`：拒绝，范围含糊，会让调用者误以为包含 MCP 与 Skill；响应实际只包含 builtin manifests。
- 只改 Rust/UI wrapper 名称并保留 `get_tools`：拒绝，IPC 契约仍与集合读取动词不一致。
- 把 Tool 管理命令继续留在 Skills module：拒绝，owner 不符，且同组命令已经包括 catalog、toggle 与 circuit 操作。

## 影响与验证

- Tauri command、响应类型名、Rust 模块路径及 UI wrapper 函数名发生破坏性重命名。wire 字段、manifest 数量与顺序、启用状态来源和用户可见行为不变。
- 不改配置或持久数据，无需数据库重置或配置迁移。
- 已执行 IPC contract 生成/一致性检查、Rust workspace 编译与 Clippy、UI 类型检查；测试未运行。

## 回滚

恢复旧 command、response type、handler module、UI wrapper、生成契约、注册表和文档名称即可；无需数据重置。
