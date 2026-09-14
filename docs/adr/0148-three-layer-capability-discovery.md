# ADR 0148：三层能力发现与按需工具详情

## 状态

已接受（2026-09-14）。本 ADR 细化 ADR 0145 的模型提示词目录交互；loader、session
隔离、权限矩阵和 provider `tools[]` schema 边界继续遵循 ADR 0145。

## 背景

Haven 的内置工具数量持续增加。把每个 operation 的名称、描述和 schema 都放进稳定提示词，
既增加上下文成本，也要求模型记住一张不适合当前任务的扁平清单。另一方面，模型在需要某个
能力时仍然需要一个可查询、可分页且不会意外执行工具的发现入口。

## 决定

模型可见的能力目录采用三层结构：

1. **第一层（提示词常驻）**：`system`、`agent`、`haven` 等 family 的整体用途、规避事项、
   root 名称和数量摘要。可选的 Skill/MCP 也只提供紧凑的名称或服务器摘要。
2. **第二层（`tool_catalog`）**：查询某个 family 下的 root，或描述一个 root（例如 `window`），
   返回 root 的整体说明和子 operation 名称/短摘要，不返回子 operation 的完整 schema。
3. **第三层（`tool_catalog`）**：用精确名称描述一个 operation（例如 `window.screenshot`），
   返回其描述、风险、指导、完整 `input_schema` 以及对应的加载提示。

`tool_catalog` 是 Safe、要求 session context 的只读控制面工具：

- `{"action":"list"}` 列出第一层 family；
- `{"action":"list","level":"tools"}` 列出第二层 root；
- 将 `level` 设为 `operations` 可列出完整第三层 operation，增加 `root` 可限定范围；
- `{"action":"describe","name":"window"}` 或 `list` 加 `root` 展开 root 的子项；
- `{"action":"describe","name":"window.screenshot"}` 查询第三层精确详情；
- `cursor`/`limit`/`next_cursor` 提供有界分页；发现动作不加载、不调用、不执行目标工具。

### 统一目录模型与状态链路

目录的唯一元数据来源是 `ToolManifest`：`identity`（`source`、`catalog_group`、
`root`、`operation`、`stable_name`）、`model`（名称、描述、schema）、`policy`（风险、
权限键、确认、幂等性、作用域、并发）、`presentation` 和 `availability`。Builtin 和 Skill
通过 `ToolDef` 生成 manifest；MCP 将 `McpToolInfo` 映射到同一字段语义，使用
`mcp__...` 的稳定 provider 名称。manifest 是 host/UI/权限目录模型，不直接塞进 provider
`tools[]`；provider schema 只由当前 session 的 `ToolDef` surface 生成。

状态严格按以下单向链路变化：

`discovered → described → load_requested → loaded(session) → executable → executed`。

`discovered` 只读 host catalog/Skill index/MCP tools cache；`described` 只返回紧凑字段，
第三层 operation 才返回完整 schema；`load_requested` 由显式 loader action 产生；只有原子
预算检查成功后才进入 `SessionCatalog`，下一轮才进入 `tools[]`。`execute` 只能从 global
core registry 或该 session overlay 查找，deferred catalog 不是可调用注册表。加载失败、
超预算、断线和权限拒绝均保持在失败状态，不隐式回退到执行或部分加载。

上下文预算分开计算：第一层 prompt 目录使用固定短文本预算，第二层/第三层 discovery
响应分别受分页 `limit`（服务端封顶）和单字段字符上限约束，provider `tools[]` 受
`context_limits.max_tools_per_request` 约束，工具 observation 仍使用独立的
`max_observation_chars`。loader 批次做 all-or-nothing admission；已加载名称不重复计数，
超限只返回可选名称摘要。

目录 list 返回 `catalog_revision`（builtin/session/MCP 三个单调时钟的组合）。继续使用
`next_cursor` 时必须回传该 revision；配置、builtin/Skill rebuild、session load 或 MCP
`tools/list_changed` 使旧 cursor 返回 `stale_cursor`，调用方从 0 重新分页。prompt 的
第一层缓存按 global registry 版本失效，MCP/Skill 变化在下一次 resume 重建；provider
schema 缓存按 `(global_version, session_version)` 失效，不让一个 session 的 load 影响其它
session。

权限在 load 和 execute 两处都成立：目录只列 enabled/available 项，loader 是 Safe 但
要求私有 session context，目标 operation 的 intrinsic policy、`permission_key` 和
disabled-operation 规则仍由统一安全网关执行；MCP/Skill 默认 High，不能借由延迟加载绕过
确认。外部 MCP/Skill 描述和 schema 的人类可读注释按单行/长度上限清洗；不信任其内容为
指令，不向 prompt 暴露 MCP command、args、env、凭据或宿主路径。schema 的结构、枚举、
默认值保持不变，执行仍使用 host 保存的原始 MCP input。

resume 只重放事件中已成功的 loader 选择（MCP server + 可选 raw tool names、builtin
operation/root、Skill name），通过同一 loader 的幂等注册路径恢复 session overlay；缺失或
禁用能力是可观测的软失败，坏 snapshot 仍 hard fail。并发 load 在 session 写锁下完成预算
检查和批量提交；失败不得留下半批次。rollback 截断事件和投影后丢弃 session overlay，
下一轮从 snapshot 选择重新加载；不把 catalog 描述写入 transcript 作为第二真源。

兼容性与迁移：本 ADR 不改数据库 schema、实体 ID 或既有权限 key；旧配置无需迁移。旧版
无法识别 loader action 时按测试版发布策略结束/重置进行中 snapshot，不保留永久兼容分支。
诊断只记录 source/name、revision、选择数、预算拒绝、连接状态和耗时，不记录 schema 中的
凭据或完整参数；关键门禁覆盖 prompt 轻量化、三层发现、describe 无副作用、load 后 schema、
Builtin/Skill/MCP 适配、分页 revision、缓存失效、权限拒绝与不可信元数据清洗。

Builtin/Skill/MCP 的完整实现仍分别由 `DeferredToolCatalog`、Skill catalog 或 MCP cache
提供。模型获得第三层详情后，必须使用 `load_builtin`、`load_skill` 或 `load_mcp` 激活能力；
只有当前 session 已激活的能力才进入 provider `tools[]`，该结构化 surface 仍是实际调用 schema
的唯一权威来源。

## UI 投影与复核修正

工具管理页不是模型的 session provider surface，因此继续使用 `get_tools` 的全量 builtin
投影（包含已禁用项），而不是调用要求 session context 的 `tool_catalog`。UI 按同一份
`ToolManifest.identity` 渲染三级树：`catalog_group` 为第一层 family，`root` 为第二层根能力，
`stable_name`/`operation` 为第三层 operation；搜索和启用状态筛选在树上逐层裁剪，操作级
Schema、风险和开关仍保持独立。MCP 与 Skill 仍分别在资源页维护，不把它们的执行配置混入
builtin 管理树。

复核三层目录的版本失效链路时发现，MCP 的渐进式 `connect_server` 路径没有安装与启动路径
相同的 `tools/list_changed` 目录版本监听，且直接插入 client 时没有递增 MCP 目录时钟。现已
收口为共享监听/注册边界；服务端新增或删除工具后，已有分页 cursor 会按既有
`stale_cursor` 契约失效。该修正不改变 wire shape、权限或加载语义。

## 未采用的方案

- **提示词平铺所有 operation**：实现简单，但上下文和记忆负担随安装能力增长。
- **只提供名称、不提供详情查询**：模型无法可靠获得精确 schema，也无法区分相近 operation。
- **查询时自动加载工具**：会改变 session provider surface，增加隐式副作用并破坏按需加载边界。
- **为每个 root 建一个发现工具**：会复制目录协议和安全处理；一个分页的 `tool_catalog` 足够覆盖
  builtin、Skill 与 MCP 三类来源。

## 影响

提示词更短且只承担方向导航；模型多一次或多次轻量 catalog 查询换取准确的 operation 选择。
`tool_catalog` 不改变数据库 schema、实体 ID、权限 key 或工具执行权限。MCP 未连接时只能列出
已配置服务器摘要；连接并发现后的缓存工具才能提供 operation 级详情，激活仍须通过 `load_mcp`。

实现边界说明：`crates/tools/src/builtin/tool_catalog.rs` 当前约 880 行，暂时将三层查询编排、
来源合并和响应裁剪放在同一私有模块，确保目录协议只有一个实现。若后续继续加入新的外部来源
并超过约 1000 行，应按来源适配器和响应渲染职责拆分，保持本 ADR 定义的 wire shape 不变。

## 验证

- `cargo fmt --all -- --check`
- `corepack pnpm --dir ui run check`
- `corepack pnpm --dir ui run test:run`
- `cargo test --locked -p haven-tools`
- `cargo test --locked -p haven-mcp`
- `cargo test --locked -p haven-agent`
- `cargo clippy --workspace --locked -- -D warnings`

## 回滚与重置

回滚代码即可恢复旧的提示词目录；本 ADR 不引入数据库或持久化格式变更，无需数据迁移或重置。
若回滚期间仍有会话在使用新的 `tool_catalog` 调用，结束该会话或按发布版本策略重置其运行状态，
避免旧版本把新的控制面工具视为未知能力。
