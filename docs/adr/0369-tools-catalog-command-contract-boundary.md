# ADR 0369：Tools catalog command contract boundary

- 状态：已采纳（2026-09-26）
- 基线：HEAD `3103a8f`；开始时工作区干净
- 范围：ToolsView 的 `get_tools`、`list_mcp_tools`、`list_skills` 与 `reset_tool_circuits` renderer command boundary
- 关联：ADR 0346（app event listener contract）、ADR 0357（memory command contract）、ADR 0368（model discovery command contract）

## 背景与审计

`get_tools`、`list_mcp_tools` 和 `list_skills` 是 ToolsView 活跃的只读 catalog commands；`reset_tool_circuits` 是同一视图唯一相邻的本地状态 mutation。这四个命令此前都被 UI 直接 `invoke`，其中 `get_tools` 还由布局启动时预热调用；列表结果没有命名 TypeScript response contract，ToolsView 把 `ToolListResponse.tools` 强转为 `Array<any>`。

`toolManifest.ts::parseToolManifest` 是现有唯一的 `ToolManifest` snake_case 到 camelCase mapper。ToolsView 先通过 `setToolManifests` 对同一行集调用该 mapper，再自行重复 map/filter 一次。MCP/Skill snapshots 当前由 cards 原样读取 Rust snake_case 字段；MCP status 使用 serde 外部标记 enum，必须接受未来 variant 和附加字段。MCP `input_schema` 与 builtin `input_schema` 是动态 JSON schema。

审计没有发现另一个 MCP/Skill response mapper、重复 Rust DTO 或调用方刷新状态 owner。MCP/Skills 的写命令、连接/刷新、授权与 `ToolRuntimeCoordinator` 生命周期均不属于此 command boundary。

## 决定

1. 新增 `contracts/tools.ts`，对照 Rust `ToolListResponse`、`ToolManifest`、`SkillInfo`、`McpServerSnapshot` 和 `McpToolInfo` 定义命名 wire contracts。保留 snake_case 字段；JSON schema 保持 `unknown`；MCP status 使用开放字符串/外部标记 object 表示，并为 additive fields 声明开放索引。
2. 新增 `toolsCommands.ts`，为四个 ToolsView command 提供 typed direct-forward helpers。helpers 不校验、转换、排序、筛选、包装结果或捕获错误；空数组、未知扩展字段、MCP status variants 与 invoke rejection 原样通过。
3. `ToolsView` 的 `get_tools`、`list_mcp_tools`、`list_skills` 与 circuit reset 通过该 helper 调用。列表刷新与初始化次序、错误处理和熔断成功通知仍归视图。
4. `setToolManifests` 继续是 builtin manifest 的唯一 runtime mapper，并返回本次已解析的行。ToolsView 用这些同一行构造 builtin presentation entries，不重复解析；Map snapshot 仍按 stable name 保存最后一项，卡片列表仍按原始顺序投影后排序。
5. `check-ipc-contracts.ps1` 对照 Rust registry/DTO 与 TypeScript command/wire fields，并拒绝视图及其他 UI source 绕过 `toolsCommands.ts`。不引入 Rust→TypeScript codegen。

## 兼容性与影响

Rust command names、无参数 payload、snake_case response fields、MCP server 排序/脱敏、Skill 与工具列表刷新顺序、事件 refresh debounce、通知顺序和现有 catch/logger/reportError 行为不变。MCP/Skill snapshots 未经 mapper，未知字段和 MCP tagged status 保留；builtin manifest 仍由原 mapper 投影，`input_schema` 的动态 JSON 原样保留，未知 manifest 顶层字段仍按既有投影丢弃。

`reset_tool_circuits` 仍是 mutation command，typed helper 只固定其 `Promise<void>` renderer 边界；MCP/Skills 写入、连接与 refresh、授权、ToolServices、数据库、ID 与 X12 均未修改。无数据、配置、schema 或重置步骤。

## 验证

新增 command helper 回归覆盖命令名、direct-forward 响应 identity、空列表、未知 MCP status/扩展字段、动态 schema 与 rejection 传播。既有 ToolsView 和 manifest tests 验证卡片投影及唯一 mapper。IPC contract script 校验响应 DTO 与 Rust 字段、typed helper 及无绕行。

```sh
corepack pnpm run check
corepack pnpm run test:run
corepack pnpm run build
pwsh -NoProfile -File scripts/check-ipc-contracts.ps1
pwsh -NoProfile -File scripts/check-ipc-events.ps1
git diff --check
```

## 回滚

回滚本提交可恢复四处 ToolsView 直接 invoke、`get_tools` 的 raw cast 与重复 manifest parsing，并删除 typed contracts、helper、IPC assertions 和本 ADR/路线图记录。无 Rust 或数据回滚步骤。
