# ADR 0549：为 UI 集合查询统一使用 list 动词

## 状态

已采纳并实施。Session wrapper 的 `listRuntimeSessions` 与工具 wrapper 的集合动词仍有效；保留 `get_sessions` / `get_tools` IPC 名称的决定分别由 [ADR 0679](0679-unify-session-runtime-and-history-terms.md) 和 [ADR 0680](0680-name-builtin-tool-manifest-list-command.md) 替代。

## 背景

`toolsCommands.ts::getTools()` 返回整个 `ToolListResponse`，`sessionHistoryCommands.ts::getSessions()` 返回整个 `SessionListResponse`；二者都没有单项 key。相邻的 MCP、Skills 集合读取已使用 `listMcpTools()`、`listSkills()`。`ChatSessionStartupDependencies` 也把全量会话集合读取口命名为 `getSessions`。项目命名规范将 `get` 用于按稳定 key 读取一项，将 `list` 用于读取集合。

## 决定

1. 将 UI wrapper `getTools` / `getSessions` 改为 `listTools` / `listSessions`。
2. 将 chat startup 注入端口及测试 harness option 一并改为 `listSessions`。
3. 保持 Tauri command 字符串 `get_tools` / `get_sessions`、返回 DTO 与后台实现不变；这些是既有 IPC 契约。

## 替代方案

- 保留函数名以匹配 Tauri command：拒绝。命令注册名是 wire API，TypeScript wrapper 名称是 UI 调用语义，两层无需使用同一动词。
- 把底层 Tauri command 改名：拒绝。不会带来 UI 语义收益，反而扩大 IPC 改动面。
- 批量改写其他 `get*` 函数：拒绝。单值读取、分页读取和状态快照按自身可观察语义保留原名。

## 影响与验证

- 仅更名 UI 内部函数和依赖端口；IPC 名称、调用顺序、数据 shape 与用户可见行为不变。
- 验证：`corepack pnpm run check`、`corepack pnpm run test:run`、`corepack pnpm run build`、`scripts/check-adr-index.ps1` 与 `git diff --check`。

## 回滚

恢复 `getTools` / `getSessions` 和 `ChatSessionStartupDependencies.getSessions` 旧名，并移除此 ADR 与当前路线图记录。无需 IPC 或用户数据迁移。
