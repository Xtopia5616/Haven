# 执行上下文 Slice 3：进程路径 capability 盘点

> 日期：2026-10-23。静态源码盘点，供执行上下文 Slice 3 验收与后续统一执行环境设计使用。此报告描述当前工作树，不是 Windows 运行时隔离验收。

## 结论

- `ProcessContainment` 在 Windows 上通过 Job Object 管理子进程树、进程数量和关闭时终止；它本身不限制文件或网络访问。
- 目前只有前台 `shell` 的 `WorkspaceWrite` 路径尝试通过 MXC ProcessContainer 落实文件与网络边界。启动前 probe 必须确认 MXC 能执行完整请求；失败时拒绝启动，不回退到 host process。
- Shell 的 `FullAccess`、MCP stdio、Skill setup/runtime、LSP、Lifecycle Hook 和 Everything CLI 使用本机权限进程。适用入口另有授权、输入校验、输出预算、取消或 Job Object 管理，但这些不等于 OS 文件/网络隔离。后台 ToolRun 复用目标工具执行器；其资源隔离状态取决于被调用工具，而不是 ToolRun 自身。
- 按 ADR 1368，Skill、MCP stdio、LSP、Everything 和 lifecycle hooks 不使用 MXC；在 WorkspaceWrite 下依现有入口策略继续拒绝或保持既有 FullAccess 限制。不能因为进程已通过 start authorization 或挂入 Job Object，就把它当作满足 WorkspaceWrite。
- URL/文件打开、开机自启、shell 可用性探测、Skill 文件夹打开、媒体目录 junction helper 属于 App/系统集成进程路径，不经过 ReAct 工具执行环境。
- 当前 `ToolCapabilitySnapshot` 汇总媒体与 Web Search 可用性，不报告子进程隔离能力。因此本表是 Slice 3 的统一 capability 审计记录；它不是进程路径的动态运行时状态 API。

## Capability 维度

| 维度 | 当前权威实现 | 可证明的内容 | 不能据此声称 |
|---|---|---|---|
| 进程树与生命周期 | `haven-platform::process_containment::ProcessContainment` | Windows Job Object、kill-on-close、活动进程上限；调用方将新进程挂入 Job 后再恢复 | 文件权限受限、网络受限、只访问当前工作目录 |
| 资源隔离 | `haven-platform::sandboxed_process::SandboxedProcess`（MXC） | MXC probe 接受当前策略后，按请求的读写/只读路径以及公网出站、私网双向访问、宿主回环三项网络权限创建 ProcessContainer | 未被 probe 证明的策略；域名级网络白名单；任何其它 host process 自动获得同等隔离 |
| 操作授权 | `haven-tools` 的 `ExecutionPolicyAuthorization` / 具体能力 owner | 对应入口在 spawn 或副作用前按其 contract 检查当前授权；MCP 远程调用另外绑定连接目标及完整参数摘要 | 授权本身是 OS sandbox；进程内第三方代码无法越过 host 权限 |
| 进程创建序列化 | `process_spawn_lock()` | Haven 自有标准进程适配器串行化短暂的 inheritable-handle 创建窗口 | 子进程访问隔离或互相独立 |
| 动态能力摘要 | `ToolsFacade::tool_capability_snapshot` → `ToolCapabilitySnapshot` | prompt/runtime 当前可用的媒体与 Web Search 能力 | MXC 可用性、各子进程当前隔离强度或一份完整的 process capability matrix |

## 子进程入口清单

| 入口 | 创建者与触发方式 | 入口级控制 | 文件/网络边界结论 |
|---|---|---|---|
| 前台 Shell，`FullAccess` | `haven-tools` `builtin/shell.rs`，当前 ReAct 工具调用 | 执行策略 permit；Windows Job Object 管理树、取消会 kill 子进程 | 按设计以当前 Windows 用户权限运行；不提供文件/网络 sandbox |
| 前台 Shell，`WorkspaceWrite` | 同上 | 执行 permit；Windows 先由 MXC probe 校验请求，再在 ProcessContainer 中 spawn；不支持或 probe 失败即拒绝 | 当前唯一接入 MXC 的进程能力；按请求设置工作目录、路径集和 typed 网络范围。MXC 路径不支持时无 host fallback；非 Windows 拒绝此模式 |
| 后台 ToolRun（包括 Shell） | Agent `start_background_tool` 将已授权目标调用连同 proof 交给 ToolRun 生命周期 owner；后台执行再经普通 Tools 授权执行入口 | ToolRun 管理持久状态、取消、会话归属和结果投递；目标工具重新校验 proof/live policy，并执行自己的文件/网络边界。Shell WorkspaceWrite 因而复用前台 Shell 的 MXC adapter | ToolRun 本身不是另一种文件/网络 sandbox；未接入目标工具 sandbox 的能力仍无 OS 资源隔离。测试专用 legacy Shell helper 不代表生产调用路径 |
| MCP stdio server | `haven-mcp` `client.rs`，连接配置启动 server 时 | 单独的 process-start permit；`ProcessContainment`；stdio 生命周期随 MCP client 管理；tool invocation 再走 MCP 远程资源授权 | 本机权限，无 MXC 文件/网络策略。启动 server 的授权和每次远程 tool call 是不同边界；stdio 进程实例的执行上下文隔离仍需按提案验收 |
| MCP Streamable HTTP | `haven-mcp` HTTP transport | 不启动本机子进程；MCP tool call 绑定连接配置指纹、连接代次和参数摘要 | 远程 endpoint/对象调用路径，不是本机进程隔离 |
| Skill 环境创建与依赖安装 | `haven-skills` `venv.rs`，激活含依赖的 Skill 时运行 Python `venv` / `pip` | 进程启动 permit；`ProcessContainment`；输出有界 | 本机权限；pip 网络和文件副作用没有 MXC 边界 |
| Skill 脚本执行 | `haven-tools` `skill_runner.rs`，模型加载后调用 Skill | 执行策略 permit；清理部分环境变量；每次 attempt 有私有 scratch cwd；`ProcessContainment`、取消及输出预算 | scratch 组织相对输出，不是文件 sandbox；Python/脚本仍以当前用户权限访问文件和网络 |
| LSP server | `haven-tools` `builtin/lsp.rs`，语言工具启动 server 时 | 执行 permit；过滤基础环境；FullAccess 用 `ProcessContainment`，WorkspaceWrite 的 Windows 入口用 MXC；进程与 server 会话管理 | FullAccess 仍按用户权限运行且要求 Open 网络；WorkspaceWrite 的 Windows MXC 将项目与运行时根设为只读、单次 LSP scratch 设为可写并关闭网络，scratch 上限 256 MiB/100,000 项。自动化覆盖策略编译与 contract，真实 server 的 MXC 端到端验收仍开放（ADR 1418） |
| Lifecycle Hook | `haven-tools` `lifecycle_hooks.rs`，匹配已配置 lifecycle event 时 | Hook process-start authorization；`ProcessContainment`；取消、时间和输出上限 | 受管理的 host process；没有 MXC 资源隔离。Hook 配置是明确的本机扩展入口，不是 Shell 子命令 |
| Everything CLI | `haven-tools` `builtin/everything_search.rs`，按文件搜索配置选择 CLI 时 | 当前搜索执行 permit；最小化环境；`ProcessContainment`；查询、数量和等待时限受限 | 工具契约可以限定搜索根，但底层 CLI 没有 MXC 文件/网络沙箱 |
| shell 可用性 probe | `haven-app-binary` `commands/settings.rs`，Settings 探测 `where.exe` / `which` | 固定 executable name、标准 spawn lock、等待退出 | 探测系统 PATH；不是模型工具调用，不提供 OS 隔离 |
| 开机自启管理 | `haven-app-binary` `autostart.rs`，用户启用/关闭自启时调用 `schtasks` | 固定子命令/参数、标准 spawn lock；结果同步等待 | Windows 用户任务计划管理；不属于会话执行能力 |
| URL/路径/Skill 目录打开 | `haven-app-binary` `commands/external.rs` 与 `commands/skills.rs`，用户操作请求系统默认处理器或 Explorer | URL/路径由 App command 校验；Windows 用 ShellExecute/Explorer；其它系统通过系统 opener；普通进程路径持 spawn lock | 将目标交给系统默认应用；目标应用不受 Haven 的 process containment 管理 |
| Managed Media junction helper | `haven-app-binary` `commands/managed_media.rs`，Windows 媒体目录维护路径 | 固定 PowerShell 脚本与已计算路径；标准 spawn lock、同步检查结果 | 本机用户权限执行文件系统维护；没有 Job Object 或 MXC sandbox |

`ProcessContainment` 还用于固定 helper 的测试子进程；该类测试启动不计入生产执行路径。Async task、线程池、`spawn_blocking` 和模型 HTTP client 是 Haven 进程内任务或网络请求，不属于本表的 OS 子进程入口。

## 补充盘点：进程表之外的执行与出网面（2026-10-25）

这部分不是新的子进程清单，而是不会随 MXC 进程策略自动收口的独立边界：

| 能力 | 当前边界 | 遗漏/后续验收 |
|---|---|---|
| `http` 工具 | 请求留在 Haven 进程内；目标解析、DNS pin、代理禁用和每跳重定向校验由 `builtin/http.rs` 执行，并使用授权 owner 提供的域名约束 | 这是应用内 HTTP 的执行边界，MXC 网络开关不会自动控制它；全局策略要通过同一个 typed 决议与执行快照传入，不能复制一套 UI 开关 |
| LLM、Embedding、STT/TTS、OCR、图片生成与联网搜索 provider | 由 `haven-llm` 的 reqwest client 发起；部分 provider 支持自定义 endpoint 和代理，ChatGPT OAuth 另有 token 请求 | 这些是宿主进程内出网，尚不能由 Shell/MXC 的进程规则隔离。统一策略需要明确哪些是用户明确配置的 provider endpoint，哪些受全局 `NetworkResourcePolicy` 约束；自定义本地 provider 需独立的显式本地目标授权 |
| 外部浏览器 / 系统 opener | App 校验并启动系统默认 URL handler；之后的 DNS、重定向、Cookie、下载和页面脚本由浏览器/系统应用处理 | Haven 可授权 URL 移交，但不能声称已对浏览器请求套用本地 Shell、MCP 或 HTTP 工具策略。若要纳入统一策略，需要浏览器扩展/代理一类独立执行面；当前没有这层证明 |
| OAuth loopback callback | ChatGPT Plan 登录启动临时 `127.0.0.1` listener 接收授权回调 | 这是有端口、生命周期和请求形状限制的本机服务，不等于允许任意 loopback/私网访问；策略模型应保留窄的本地服务例外 |
| WebView 前端请求 | 对 `ui/src` 的生产源码静态搜索未发现 `fetch`、XHR、WebSocket 或 EventSource 网络请求；前端主要通过 Tauri IPC 访问 Rust owner | 当前没有发现独立的 renderer 任意出网入口；此结论是静态搜索结果，不替代未来插件/WebView 导航与 CSP 审计 |
| MXC 子进程网络 | Shell request 分别表达公网出站、私网双向访问和宿主回环；MXC 原生策略仍没有域名、端口、协议或指定私网 endpoint 字段 | 不能据此实现域名白名单、只允许 HTTPS 或只访问某个本地 provider；三项开关不互相推导。受限模式继续对 opaque 子进程拒绝，Open 与具体本地目标也必须区分 |
| 本机 IPC / 句柄 | Job Object 与网络 profile 不描述 named pipe、COM/RPC、继承句柄或其它本机服务通信 | 对 MCP、Skill、Hook 等不可信或可配置代码，需要评估本地 IPC 是否成为文件/网络策略旁路，并测试 MXC backend 的实际限制 |

因此遗漏项分为三组：需要评估是否能复用 MXC 的本机子进程（Skill、LSP、MCP stdio、Hooks、Everything）；必须共享授权与目标决议但由 Haven 进程内 transport 执行的 provider/HTTP 请求；由操作系统或外部浏览器拥有、不能由 Haven 进程 sandbox 直接强制的访问。后台 ToolRun 应复用目标工具的执行策略，不额外发明 sandbox。全局策略可统一前两组的策略来源和决议契约，但仍需把外部浏览器明确标成外部执行边界。

## Slice 3 的实现边界

本次盘点不把每个受管理的子进程改为 MXC。只有能够将入口的真实文件/网络请求完整表达为平台 policy，并在 spawn 前通过 backend probe 验证时，才可使用受限进程模式；其它入口继续按其现有授权运行在本机权限下，并在 capability 描述中如实区分 process-tree control 与资源隔离。MXC 拒绝、不支持的资源类型或策略变化都必须 fail closed，不能退回 host spawn。

下一步若要将这份静态矩阵变为运行时 capability API，应由 Platform 提供描述本机 backend 与支持策略的窄 snapshot，并由 Tools 将其与具体路径所需能力匹配。该 API 不应把 `ProcessContainment` 的 Job Object 状态投影成文件/网络 sandbox，也不应让消费者取得 platform manager/registry。此报告未引入该 API、未改变运行行为或持久契约。

## 验证范围

本报告基于当前工作树静态搜索与调用链阅读；它没有证明每种真实 Windows host policy 或 MXC backend 的运行状态。Windows MXC 负向行为与发布验收仍需在 Windows 环境按[路线图 Gate](architecture-refactor-roadmap.md#51-windows-发布验收gate--open)执行。

## Windows 运行时验收补充（2026-10-24）

Slice 3 已在 Windows 11 专业版 Insider Preview `10.0.29683` 的真实 host 上完成 MXC BaseContainer 验收。显式执行 `cargo test --locked -p haven-tools workspace_write_shell_ -- --ignored --nocapture --test-threads=1`，4 项通过：workspace 根内写入成功、兄弟目录写入拒绝；宿主 loopback 对 `Deny` / `Restricted` 拒绝、对 `Open` 允许；MXC 沙箱内子进程在取消与授权撤销时都被终止。PowerShell 默认 UI 策略曾导致 `STATUS_DLL_INIT_FAILED`；Shell 现在仅为 PowerShell 选择 MXC desktop UI isolation，并保留剪贴板、输入注入、系统设置、桌面控制和 IME 限制。平台策略单测 3 项通过。MXC probe 或 spawn 出错仍直接返回失败，无 host shell fallback；当前支持主机的 backend-unavailable 分支未被人为制造，已复核错误传播控制流。

Rust 全 workspace 测试与 strict Clippy 均通过。该运行时补充不把 Job Object、Skill、MCP stdio、LSP 或其它 host process 误报为 MXC 资源隔离；它只证明前台 Shell `WorkspaceWrite` 的这组实际策略。完整安装/升级/卸载、数据保留与 ENOSPC 仍属于独立 Windows 发布 Gate，见[路线图 §5.1](architecture-refactor-roadmap.md#51-windows-发布验收gate--open)和 [ADR 1382](adr/1382-execution-path-capability-audit.md)。

## Windows 网络方向权限复验（2026-10-25）

MXC 网络权限改为独立的公网出站、私网双向访问与宿主回环字段后，平台单测验证公网出站不会推导私网/回环访问；真实 Windows MXC 测试 `workspace_write_shell_keeps_host_loopback_separate_from_public_egress` 通过。该测试确认 Shell `Open` 请求允许公网出站，但私网与宿主 loopback 仍拒绝。上文 2026-10-24 的 loopback 结果是旧的布尔策略行为，不代表当前策略。详见 [ADR 1417](adr/1417-separate-mxc-public-egress-from-local-network.md)。
