# ADR 0369：Tools catalog command contract boundary

- 状态：已采纳（2026-09-26）
- 基线：HEAD `3103a8f`；开始时工作区干净
- 范围：ToolsView 的 catalog、MCP/Skills 管理与 refresh renderer command boundary
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

## 2026-09-27 后续：MCP/Skills 管理命令边界

### 审计结果

ToolsView 除 catalog/read/reset 外还直接调用 `refresh_mcp_servers`、`reconnect_mcp`、`add_mcp_server`、`update_mcp_server`、`remove_mcp_server`、`toggle_mcp_server`、`set_skill_enabled`、`set_tool_enabled`、`refresh_skills` 和 `open_skills_dir`。此前共用的 optimistic toggle 以运行时命令字符串调用 `invoke`，静态命令检查无法识别该绕行。所有活跃调用现通过 `toolsCommands.ts`；view 仍拥有 optimistic state、刷新顺序、通知与 catch/reportError/logger 行为。

MCP status event 继续由 Rust command/admin owners 发出，ToolsView 的既有 listener 负责 debounced MCP snapshot refresh；Skills status event 仍触发 Skills list refresh。Refresh button、单服务器 reconnect、保存/删除/启停后的显式 snapshot refresh、Skills refresh 后的 list refresh 均保留原顺序。helper 不接管或重复这些副作用，也不转换响应或捕获 rejection。

MCP add/update/remove/toggle 与 Skill/tool enable 通过 `authorize_admin_request` 和既有 native admin operations；`open_skills_dir` 在 handler 内走 AuthorizationEngine。`refresh_mcp_servers` 与 `reconnect_mcp` 会对已持久化的 MCP 配置发起连接副作用，但它们的 Tauri handlers 本身不请求 AuthorizationEngine；`refresh_skills` 扫描已配置的目录且不要求确认。此切片仅记录并保持这些现存授权边界，不增加或重排授权。针对 renderer 可调用的 refresh/reconnect 是否需要统一授权策略，留给单独安全决策与行为测试，避免藏在 typed IPC 迁移中。

### 决定

1. 为上述命令增加 typed direct-forward helper，request/response types 使用 Rust wire 的 snake_case 字段。`McpServerConfig` 对齐 Rust 固定 DTO 和 `stdio`/`http` enum；不接受任意 config 属性，也不另造 parser。动态 schema 仍只在既有 `ToolSchema = unknown` 扩展边界。
2. `McpRefreshResult` 使用命名 response DTO，四个结果数组与未来附加字段透传；refresh helper 不发起额外调用。add/update 的 config 和 name 参数依 Rust handler 的 `config`、`name` 顶层参数传递。
3. 将 optimistic toggle 改为接收命名 helper 回调，删除动态命令字符串 `invoke`。IPC contract script 对照 Rust registry、handler 参数、config/refresh DTO、helper 签名与响应类型，并禁止 ToolsView 直接或动态 `invoke` 绕行。
4. `refresh_mcp_servers` contract 描述 renderer-triggered persisted-config reconcile，明确不接收 renderer 的 process arguments。它仍是活跃 ToolsView command，按既有 handler 行为运行，未被加入 AuthorizationEngine。

### 兼容性、验证与回滚

不改 Rust handler、wire payload、事件生产者、MCP connect/reconnect 时序、authorization owner、配置文件格式、数据库或数据重置要求。UI command failure 继续原样抛给现有 handler catch；通知文案和 refresh 顺序不变。回滚时恢复旧 command invocation 所在位置、删去新增 helper/contracts/script guards，并恢复本节前的历史边界描述；无需数据迁移或重置。

验证覆盖 MCP config/refresh 响应透传、所有写命令参数、代表性 rejection 传播、contract drift 与 UI 绕行检查；门禁结果追加在提交记录和路线图。

验证结果：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings` 与隔离配置下的 `cargo test --workspace --locked` 通过；UI `check` 无错误/警告，112 个 Vitest 文件的 836 项测试通过，生产 build 成功；`check-ipc-contracts.ps1` 的 registry、Tools DTO/helper 断言及 `check-ipc-events.ps1` 均通过。Rust 测试使用新建的 `target/test-data/tools-mutation-ipc-20260927-run02/APPDATA`，没有访问真实用户配置。修改过的常规 UI 文件 Prettier 检查通过；`contracts/commands.ts` 保留仓库既有的紧凑 registry 行布局。

### 2026-09-27 后续：renderer MCP 连接授权

此前关于 `refresh_mcp_servers` / `reconnect_mcp` 未经过 `AuthorizationEngine` 的记录是审计时的历史状态，已由本切片收敛。两条 Tauri handler 现构造 renderer 专用 typed native admin request，并复用 `authorize_admin_request` 与已有 confirmation receipt 队列；Tauri 参数和返回 DTO 不变。未授权或等待确认期间不连接、不重连、不断开。

`reconnect_mcp` 授权单个当前已启用且存在 live client 的 server，执行时在共享配置 gate 内复核 ConfigService version、enabled config 与 live client config。`refresh_mcp_servers` 依据当前持久化配置与 live-client diff 形成一个 batch plan，计划只含 config version、受影响 server 名称和 connect/reconnect/disconnect action；命令、URL、参数与环境变量不进入计划。确认后服务在共享 gate 内重算 diff 并要求 version 与完整 target set 匹配，再执行已有 diff-only 连接流程，不转用全量 `mcp_reload`。

两个操作共用 `haven_mcp` typed risk/metadata 和 AuthorizationEngine 网络能力分类。需要建立连接的 reconnect/refresh 使用 opaque network capability，因此 `NetworkPolicy::Deny` 在授权阶段阻断，`Ask` 进入确认；仅断开目标不要求网络能力。确认摘要只显示单服务器范围或 batch 各 action 数量，不暴露服务器配置。安全回归矩阵覆盖拒绝/确认、计划隐藏 renderer schema、stale plan 在副作用前失败，以及 IPC contract 对 handler 授权和原有 wire DTO 的约束。

### 2026-09-27 后续：确认后的 MCP refresh 部分失败

直接调用 `refresh_mcp_servers` 时，命令把每个连接失败名称保留在原有 `McpRefreshResult.failed` 响应中；ToolsView 用该 DTO 展示批次结果。需要确认的调用先以既有 queued-confirmation IPC rejection 返回，因此原调用方已结束，resolver 不能依赖该 DTO 通知确认后的部分失败。

确认 resolver 保留执行后的 `ToolResult` 并仅对该确认恢复路径调用 confirmed-only finalizer。finalizer 从授权 plan 与结果 `failed` 数组中提取唯一、非空且属于本批 connect/reconnect target 的名称，并通过已有 `mcp:status_change` 发布 `Offline` 状态；状态错误只使用固定通用连接失败摘要，不携带底层错误、命令或配置。既有布局监听器显示错误通知，ToolsView 的同一 channel listener刷新 MCP snapshot。空结果、畸形数组、未授权目标和直接 command 响应均不会触发此事件，因此直接调用继续由原结果 DTO 汇总，避免重复通知。无需新增 IPC channel 或改变 command DTO。
