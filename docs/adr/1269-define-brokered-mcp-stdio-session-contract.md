# ADR 1269：定义受限 brokered MCP stdio session 契约

## 状态

已采纳为后续实现契约；当前 stdio MCP 在 `WorkspaceWrite` 下仍于授权阶段拒绝，本文不开放任何新执行路径。

## 背景

Haven 当前由 `haven-mcp` 在宿主启动 stdio server，并将其作为长生命周期 client generation 管理。Windows `ProcessContainment` 负责整棵进程树的回收，但不限制同一用户身份下的文件或网络访问。现有 HCS port 面向单次 Shell 与固定 Python Skill；Skill 的 runtime、无网络 profile 和输入/输出所有权均不能代表通用 MCP server。

MCP 2026-07-28 规范继续定义 stdio 为 client 启动的子进程，以标准流传送换行分隔的 JSON-RPC；规范也允许可靠双向字节流上的自定义 transport，并要求复用 stdio framing 和为进程启动、`stderr`、关闭、重启定义等价生命周期。[stdio transport](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio)、[transport overview](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports)。现代版本把 protocol version/capabilities 放进每个请求的 `_meta`，旧版本仍使用 `initialize`，所以 client generation 仍需保持一个 server 进程生命周期以兼容旧协议。[versioning](https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning)

## 决定

- 隔离后的实现仍由 `haven-mcp` 持有 JSON-RPC 编解码、版本探测、请求/通知关联、资源/工具目录与 tool-call 取消。HCS broker 只托管 server process，并提供可靠、有界、双向字节流；它不解析或重写 MCP JSON-RPC。该适配是 Haven 的 brokered stdio-compatible transport，不宣称 guest 进程属于宿主启动的本地 stdio 子进程。
- 每个 MCP client generation 对应一个 HCS server process generation。不能把 legacy `initialize` 语义改成每次工具调用都重启 server；MCP `tools/call` 的取消仍通过 `notifications/cancelled` 转发，generation shutdown 则先关闭 guest stdin、在有界宽限期内等待退出，之后终止整棵 guest process tree。配置目标、sandbox/network policy 或授权 generation 变化时，旧进程先被撤销和清理，再允许基于新 snapshot 显式重连。
- broker 请求只能携带经验证的 runtime profile、受管理 bundle identity、bundle 内相对 entrypoint、有界 argv/env 数据和有界生命周期参数。不得传递用户配置中的宿主 `command`、宿主 `cwd`、绝对文件路径、shell command string、任意 HCS JSON 或可由模型覆盖的运行时选择。runtime 必须来自 broker 安装时固定的 profile catalog；MCP server bundle 是只读输入，不能复用 Skill 的 Python runtime/profile。
- stdio server 环境从最小 runtime baseline 重建，只追加已验证、长度受限的配置变量；不继承宿主环境。secret 值不得进入 Debug、tracing、错误文本、IPC 日志或 UI；MCP 客户端仍对 server 描述、schema、stderr 与结果应用现有外部数据处理和有界输出规则。`stderr` 是诊断流，不能单独判定 tool-call 失败。
- 首个隔离 profile 不接收 `ExecutionWorkspace`、用户目录或 host path 映射，只能写入 attempt/server 私有 guest scratch。这样 stdio MCP 在该 profile 下没有宿主文件变更可晋升，不产生 workspace review；需要读写项目文件的 server 继续拒绝，直到单独定义 per-call workspace snapshot、server 可见性及每个 MCP tool step 的 review/晋升关联。网络外部副作用仍按 MCP operation 的当前逐次授权执行。
- 网络边界独立于进程隔离。无网络 profile 明确禁用 egress；使用不透明网络的 profile 只能在 `NetworkPolicy::Open` 且 HCS 网络接入有明确实现和运行时证明时开放。`Ask`、`Restricted`、`Deny` 均不能授权一个无法按目标验证的 stdio server。broker 不接受模型参数指定网络、接口、路由或代理。
- 每个工具调用仍需通过现有 Tool proof、target binding、风险/确认和逐次网络策略；server generation 的启动 permit 只授权创建该进程，不代替调用 proof。`direct`/`deferred`/`codemode`/`hidden` 仅决定目录可见性和加载时机，不能改变该 execution contract。CodeMode 与 Workflow 子调用继续经同一 Tools 授权入口。
- 在 runtime catalog、bundle 安装/校验、network gate、server 资源配额、双向流容量/背压、取消/关闭/撤权回收以及 Windows 负向验收都闭合前，现有 `HostProcess` 绑定保持不变，`WorkspaceWrite` 下 stdio server 不得 spawn。执行 workspace 的可写 MCP profile 是后续独立设计，不以 FullAccess 或 host spawn 作为隐式回退。

## 替代方案

- 复用 Skill Python profile：拒绝。MCP server 的 runtime/package、双向协议、长期进程及可选网络行为与一次性无网络 Skill 不同。
- 每次 MCP tool call 新建并退出 server：拒绝。它会改变 legacy 进程级 `initialize` 状态，也破坏 server process generation 的目录、通知和请求取消语义。
- 只用 Windows Job Object 包装现有宿主 spawn：拒绝。它能终止进程树，但不形成文件系统或网络 sandbox。
- 把 workspace snapshot 长期挂载给 MCP server：拒绝。长生命周期进程可在任意 tool call 之外写入；按单个 `step-*` 做输出归属与晋升将失去可证明的副作用边界。
- 把 HCS broker 变成 MCP protocol client：拒绝。这样会重复 `haven-mcp` 的 framing、版本协商、关联和取消逻辑，造成两个 protocol owner。

## 影响、配置与回滚

本 ADR 只定义实现顺序与 runtime 边界，没有修改 MCP 配置、broker wire protocol、数据库或 IPC，也没有数据迁移。后续若将 `{command,args,cwd}` 配置改为受管理的 runtime/bundle 引用，将按测试版本重置规则同步更新配置 schema 和 `docs/release-and-reset.md`；不会把旧宿主命令自动解释成隔离 guest 程序。

实现时先增加 broker/MCP transport 内部接口和 fail-closed 负向测试，再逐步接入可验证的 runtime、流生命周期和 Tools 授权；任一 gate 不通过均保持 `WorkspaceWrite` 拒绝。回滚时删除 brokered stdio generation 与 transport，恢复由现有 client 连接路径处理 FullAccess stdio；无需恢复本 ADR 未引入的持久数据。

## 验证

- 本 ADR 的设计检查确认当前 `McpClient` 以 generation 持有 stdio 进程、当前 HCS port 仅有 Shell/固定 Skill 单次请求，以及 `WorkspaceWrite` 对 `HostProcess` 的拒绝路径。
- 规范核对采用 MCP 2026-07-28 官方 stdio transport、transport overview 与 versioning 文档；验收实现时还需覆盖现代与 legacy server、JSON-RPC framing、取消、EOF shutdown、超时强制回收、资源/背压、策略撤销、无 host path 与网络 fail-closed。
- 本切片仅变更 ADR 与路线图；按纯文档门禁运行 ADR 索引/本地链接检查及 `git diff --check`。未启动容器、guest MCP server 或 HCS 验收环境。
