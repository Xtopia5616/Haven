# ADR 1163：统一工具暴露、CodeMode 与受限工作流的执行边界

## 状态

已采纳；按 `docs/architecture-refactor-roadmap.md` 分阶段实施。

## 背景

Haven 已有 host-owned deferred catalog、`tool_catalog`/`load_mcp`、每请求工具预算、工具级授权、确认收据、文件/网络策略和 `SandboxMode`。当前 MCP 工具主要通过 provider 原生 tool call 执行；MCP 的工具列表之外，客户端尚无完整的资源读取与 prompts 使用面。工具的上下文暴露方式、执行机制和授权边界分布在配置、Tools、Agent 与 UI 中，尚无单一契约描述它们如何组合。

Pi 将 MCP 工具暴露细分为 `direct`、`deferred`、`codemode`、`hidden`，并以 CodeMode 让脚本在 QuickJS 沙箱中组合工具调用。CodeMode 没有 Node API、文件系统、网络或 timer；工具调用仍是真实副作用，脚本失败不会撤销已完成的调用。Pi 同时区分 MCP tools、resources 和 user-selected prompts。参见 [Pi MCP 文档](https://pi.dev/docs/latest/mcp) 与 [Pi CodeMode 文档](https://pi.dev/docs/latest/codemode)。

截至 2026-10-15，MCP 最新正式协议版本为 `2026-07-28`。该版本将协议核心改为无状态请求，定义现代 Streamable HTTP 的方法/名称路由头、可缓存列表结果和 Multi Round-Trip Requests；并弃用 Roots、Sampling、Logging 与旧 HTTP+SSE 传输。客户端后续演进应按该版本验证，不再以旧版 session handshake 作为新能力的设计基准。参见 [MCP 2026-07-28 发布说明](https://blog.modelcontextprotocol.io/posts/2026-07-28/) 和[协议变更记录](https://modelcontextprotocol.io/specification/draft/changelog)。

## 决定

### 1. 分开暴露策略、执行模式和授权

- **暴露策略**决定 capability 如何进入模型可见 surface；它不是授权，也不能批准副作用。
- MCP server 有一个默认暴露策略，并可按 MCP tool name 配置覆盖。四个策略的语义固定为：
  - `direct`：具体 tool schema 进入 provider 的原生工具列表。
  - `deferred`：保持 Haven 现有目录搜索、描述和显式加载流程；加载后使用 provider 原生工具调用。
  - `codemode`：该 server/tool 只能经受限 CodeMode 脚本调用，不作为顶层 provider tool 暴露。
  - `hidden`：不进入 prompt、目录、CodeMode namespace 或原生工具列表；后端执行入口也拒绝对它的调用。
- 单项覆盖按确定顺序解析：精确名称优先；否则匹配非 `*` 字符最多的 pattern；同长度冲突按 pattern 字典序升序取首项。pattern 只支持 `*`，不支持正则。运行时以同一个 resolver 计算目录展示、声明和执行可达性。
- 默认 server 策略为 `deferred`，使现有小工具目录和显式加载行为继续可用。切换到 `direct`、`codemode` 或 `hidden` 必须明确显示其模型可达范围和安全含义。
- `tool_settings.disabled_operations`、拒绝规则、sandbox/network policy 和后端授权始终可以进一步阻止调用；暴露策略绝不能使隐藏、禁用或拒绝的 capability 重新可达。

### 2. 保留 Legacy 路径，并将 CodeMode 做成显式可切换的替代调用面

- `direct` 与 `deferred` 走现有 ReAct/provider-native tool-call 路径（Legacy）。`codemode` 暴露要求用户在 MCP server/tool 设置中明确选择该值。两种调用面按 capability 混用；一次会话可使用原生工具和 CodeMode，但同一 capability 不会因为 CodeMode 存在而自动提高暴露等级。
- CodeMode 只接受受限脚本输入，不提供宿主文件系统、进程、环境变量、网络 socket、动态模块加载或时钟/timer API。脚本只能通过 typed tool bridge 调用当前策略允许的 capability。
- 每个子调用重新经过 Tools 的标准执行与授权边界，并继承 cancellation、确认、sandbox/network 检查、并发策略和单次结果限制；CodeMode executor 不能拿到可绕开这些检查的裸 adapter 或 manager。
- 限制脚本字节数、执行期限、最大子调用数、并行度、累积 observation 字符数和最终返回大小。预算耗尽时停止新调用并收束已完成结果；调用已产生的外部副作用不回滚。确认必须针对具体子调用发起，不能由批准外层脚本推导出对子调用的 blanket grant。
- 每个脚本执行使用独立 run-local 状态；跨脚本状态只有经显式、有界的 workflow/workspace 存储契约后才能持久化。不要把 QuickJS heap、ReAct scratch 或 MCP server session 当持久权威。

### 3. MCP resources 与 prompts 使用独立交互面

- Resources 是应用控制的上下文读取，经过 URI scheme/server 身份校验、用户可见来源、内容长度和数据敏感级别检查后，由用户或受控的 host policy 选择性附加；不把任意 `resources/read` 自动变成模型可调用副作用工具。
- Prompts 是用户选择的模板入口。读取 `prompts/list`/`prompts/get` 不意味着服务器返回内容可覆盖系统或 developer 指令；返回文本作为不可信外部数据进入有界的 user/context 段。
- 资源和 prompt 操作使用独立 typed DTO、UI 展示及授权规则，不复用 tool-call 的 schema 或 permission grant。资源读取遵守同一网络目的地策略、server auth context、缓存范围和输出预算。
- 对 2026-07-28 的 cache hints 只在 server/auth identity 一致且 `cacheScope`/TTL 可验证时使用；MRTR 的 `input_required` 在 Haven 定义明确的交互、durable event 和 resume 生命周期前保持 fail-closed，不透传成自由 form 或自动重试。

### 4. Workflow 是受限的声明式编排，不是可保存的任意脚本

- Workflow 以有版本的声明式步骤图定义，步骤引用 capability identity，参数映射只允许字面值及显式结果字段引用；禁止 `eval`、任意表达式、循环、动态 tool name 和隐式重试。
- 验证时拒绝未知/隐藏/禁用的 capability、循环依赖、超过节点/深度/载荷/输出/并发预算的图。运行时每一步走与 Legacy/CodeMode 相同的 Tools 执行、授权与安全网关；某步需要确认时按现有交互生命周期暂停该步骤，而不是先执行后补问。
- 需要跨 turn 或脱离当前 session 的 workflow 才写入 durable `ToolRun`；短 workflow 保持 run-local。新增持久定义和状态前先证明 ToolRun/interaction event owner 可表达恢复、取消、重试和部分副作用，不另造平行 job 生命周期。
- 任意 CodeMode 脚本不是 durable workflow，不能自动保存、定时执行或获得跨 turn 授权。

### 5. 权限、sandbox 与执行 workspace 共用一条后端边界

- 保留 `AuthorizationEngine` 作为授权唯一 owner。先审计并补齐现有 operation policy、capability scope、target binding、permission receipt 和 `SandboxMode` 对 MCP/嵌套调用的覆盖；不新增一套 Codemode-only 权限系统。
- capability name/exposure、permission key、目标资源、网络目的地、文件路径及数据敏感级别分开表达。用户确认绑定具体子调用规范化参数、能力、目标、策略 revision 和期限；授权重检发生在副作用前。
- 定义 `ExecutionWorkspace` 为后端执行上下文，明确规范化根目录、读/写能力、temp/scratch 路径、生命周期和 generation。它与 UI 页面导航用的 workspace/tab 完全不同；不能由 renderer 传入一个未经验证的路径来扩大 sandbox。
- 文件、进程、注册表、系统设置和网络仍由 Tools/Platform 的安全网关拥有。MCP stdio child process、远程 MCP 请求、CodeMode evaluator 和 workflow 都必须经过各自已有的 Windows Job Object、destination validator、文件句柄/path validator 与 cancellation 生命周期；workspace 不能暗示对外部 MCP server 的数据操作已被本机文件沙箱限制。

## 实施顺序

1. MCP exposure 配置与统一 resolver：server 默认值、单 tool 覆盖、预算、目录可见性和 backend hidden/disabled 拒绝；先交付 direct/deferred/hidden 的垂直调用链。
2. 盘点并收敛嵌套 capability 所需的授权、确认与 sandbox port；在完成负向/竞态测试前不启用 CodeMode。
3. 加入 CodeMode evaluator、工具 discovery/typed bridge、Legacy/CodeMode per-server 切换和脚本/调用/输出预算。
4. 按 MCP 2026-07-28 评估并实现受限 resources/read 与 user-selected prompts；明确现代 HTTP/stdio 的协商、缓存和交互支持范围。
5. 增加声明式 Workflow DTO、验证器和同步执行垂直链；再根据需求决定持久定义与 ToolRun 恢复。
6. 统一 ExecutionWorkspace 和各类 sandbox policy 的配置、来源及 Windows 实际边界，清理重复/过宽授权入口。

每个步骤单独更新本路线图、ADR/contract、负向测试和 reset/release 说明，完成适用门禁后独立提交。不得把多步实施塞入一次提交。

## 替代方案

- 只扩展 `load_mcp` 继续按 server 激活：不能明确支持常用小 server 的 direct 使用、隐藏策略或 CodeMode-only server。
- 将所有 MCP 工具强制切换到 CodeMode：拒绝；小型、确定性工具仍适合原生 tool call，且迁移会改变调用与 transcript 语义。
- 让 CodeMode 直接拿到 MCP manager/client：拒绝；会绕过已有授权、工具结果约束和生命周期 owner。
- 将长 workflow 实现成持久 JS：拒绝；任意程序持久执行扩大文件、网络、恢复和授权面，也无法表达稳定的数据/副作用语义。
- 用 filesystem workspace 同时表示 UI workspace、shell cwd、MCP process cwd 和权限范围：拒绝；这些身份和安全边界不同。

## 影响与迁移

第一条垂直切片已落地：`McpServerConfig.exposure` 管理 server 默认模式和 per-tool pattern；Common resolver 同时驱动 Direct provider surface、Deferred 目录/加载、Hidden 目录过滤和后端 stale-call 拒绝。配置经 IPC 暴露到 MCP 设置 UI，旧配置缺少该字段时采用 `deferred`，不涉及数据库 schema。模式切换不触发 MCP 重连，只刷新工具目录。

CodeMode 的 enum 和 pattern resolver 已有契约，但执行器尚未开放。当前 UI 不允许新选 CodeMode（只在已有配置中保留并允许改回），Legacy 的目录、加载和执行入口对 CodeMode fail closed。必须先完成 §5.8 的嵌套授权与 sandbox 审计，再实现 evaluator、预算和按 server 切换。workflow、MCP resources/prompts 和 ExecutionWorkspace 仍按实施顺序等待各自垂直切片。现有数据库的 `session_events`、tool-call identity、ToolRun completion/outbox 和 X12 投影契约保持权威。

## 验收要求

- 每一种 exposure 的 prompt/list/load/call 负向与正向测试；通配冲突解析顺序确定；隐藏/禁用 capability 的 stale call 由后端拒绝。
- CodeMode 尝试访问每一种不存在的 host API 时拒绝；嵌套调用的授权拒绝、确认、过期/撤销、超时、并发上限、部分副作用、取消和输出截断均有测试。
- Workflow 拒绝 cycle、越权引用、动态路由、预算超限和错误恢复；每个子步骤保留独立授权结果。
- Workspace 测试覆盖规范化、重解析点、UNC、相对路径、目标路径、竞态、进程树取消及 UI 字段无法扩大范围；依赖 Windows 行为的验收在 Windows 环境完成。
- MCP resources/prompts 按支持的协议 revision 测试 auth/cache boundary、字节上限、错误隔离；未实现的 MRTR/deprecated feature 明确拒绝并报告能力差异。

## 参考

- [Pi MCP](https://pi.dev/docs/latest/mcp)
- [Pi CodeMode](https://pi.dev/docs/latest/codemode)
- [MCP 2026-07-28 发布说明](https://blog.modelcontextprotocol.io/posts/2026-07-28/)
- [MCP draft changelog](https://modelcontextprotocol.io/specification/draft/changelog)
