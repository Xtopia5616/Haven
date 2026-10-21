# Haven 架构与 crate 职责

> 版本: v1.32 | 日期: 2026-10-20
> 范围: `crates/` (Rust 后端, Tauri 2)
> 原则: **依赖单向、叶子优先**。上层 crate 只依赖下层，绝不反向依赖；共享数据与类型放叶子（`haven-common`），
> 组件职责按「谁拥有实现、谁只消费接口」划分。

---

## 1. 依赖图

```
haven-app-binary（组合根 / 宿主边界）
├── haven-agent（ReAct 编排）
├── haven-input（输入采集 / VAD）
├── haven-tools（工具执行）
├── haven-memory（持久化）
├── haven-llm（模型与媒体 provider）
├── haven-common（跨层契约）
└── haven-platform（操作系统适配）

haven-platform ──► haven-common（credential store 端口）
haven-container-broker ──► haven-platform（Windows broker service host）

haven-agent ──► haven-tools, haven-memory, haven-messaging, haven-llm, haven-common
haven-tools ──► haven-input, haven-mcp, haven-memory, haven-messaging, haven-skills, haven-llm, haven-common, haven-platform
haven-mcp   ──► haven-common, haven-platform
haven-messaging ──► haven-common
haven-input / haven-llm / haven-memory ──► haven-common
haven-skills ──► haven-common, haven-platform
```

实际依赖（见各 `Cargo.toml`）：

| crate | 依赖 | 说明 |
|---|---|---|
| `haven-common` | 无内部依赖 | 纯叶子，全 workspace 共享；`usage` 子域拥有跨层用量契约和值枚举 |
| `haven-platform` | `haven-common` | CredentialStore 端口、共享 filesystem metadata 安全检查；Windows 凭据管理器和子进程适配 |
| `haven-container-broker` | `haven-platform` | Windows SCM service host；将受限 IPC 请求交给 platform-owned HCS broker service |
| `haven-llm` | `haven-common` | 只依赖共享层，不依赖任何业务 crate |
| `haven-memory` | `haven-common` | 持久化（当前 SQLite schema、版本拒绝与 reset contract、仓库） |
| `haven-skills` | `haven-common`, `haven-platform` | 技能目录解析、变更检测与 venv 子进程 containment；Skills root API 拥有目录变更判断，`venv` 是私有实现模块 |
| `haven-messaging` | `haven-common` | 消息服务与 inbox transport |
| `haven-mcp` | `haven-common`, `haven-platform` | MCP 客户端 / 传输；不依赖消费方接口 |
| `haven-input` | `haven-common` | 录音 / VAD / PCM/WAV 采集（不实现 provider 或转写） |
| `haven-tools` | `haven-common`, `haven-input`, `haven-llm`, `haven-mcp`, `haven-memory`, `haven-messaging`, `haven-platform`, `haven-skills` | 工具注册表 + 各内置工具 |
| `haven-agent` | `haven-common`, `haven-llm`, `haven-memory`, `haven-messaging`, `haven-tools` | ReAct 循环 + 会话执行 |
| `haven-app-binary` | `haven-agent`, `haven-common`, `haven-input`, `haven-llm`, `haven-memory`, `haven-platform`, `haven-tools` + tauri | 装配 + Tauri 命令 + 事件桥 |

> 表格是内部 crate 直接依赖的校验基准；`scripts/check-crate-dependencies.ps1` 从此表读取期望边，
> 并与 Cargo workspace metadata 做精确比对。外部依赖不列入内部边集合。依赖图是该表的概览。
> 依据 `crates/*/Cargo.toml` 实际 workspace 依赖整理。`haven-agent` 与 `haven-app-binary` 是最上层，
> 其余全部是它们的底层依赖。`haven-llm` 不依赖 MCP、Tools 或 Agent；LLM-owned 的 STT MCP caller
> 由 Tools 装配 wrapper 适配到 `McpManager`。语音转写运行时调用路径仍是 app → tools → llm；
> `haven-input` 只产出采集结果，不直接依赖 `haven-llm`。

`haven-platform` 只依赖 common 中稳定的 `CredentialStore` 端口与 credential reference validator，
不依赖 Tools、MCP 或 Tauri。Windows adapter 用 Credential Manager 保存密钥；其他平台对带凭据配置
明确失败，不退回明文或进程内持久化。该 crate 还拥有 MCP stdio、Shell、Skill script/venv bootstrap 和后台 ToolRun
子进程共用的 `ProcessContainment`。Skill 脚本和 venv/pip 准备命令各自清空继承环境后只重建所需白名单，减少环境变量泄露；这不能限制子进程读取用户文件或访问系统资源。Windows 使用 kill-on-close Job Object，并要求以 suspended
状态创建进程、先分配 Job 再恢复唯一初始线程；其他平台保持原有 no-op 行为。平台 crate 拥有该
操作系统顺序和 FFI，adapter 仍拥有命令配置、管道、取消、等待与工具生命周期（ADR 0513、0515）。
Skill 依赖安装禁用 pip 用户缓存，避免在 Haven 管理目录之外留下无容量 owner 的重复包缓存（ADR 1176）。
Skill 脚本快照位于独立 Temp 子目录，`TempPath` 负责正常回收；runner 初始化时用目录 capability 清除超过最长脚本期限的 Haven 快照（ADR 1177）。每个 composition-owned `SkillRunner` 最多并发执行 4 个 Skill，克隆共享同一执行槽；槽位覆盖快照、venv 准备、子进程和输出捕获，并将排队时间计入调用期限。由此每个 runner 同时持有的脚本快照最多 64 MiB、管道原始保留缓冲最多 64 MiB。每棵由 `ProcessContainment` 管理的 Windows 子进程树最多有 256 个活动进程，并在 Job 关闭时整体回收（ADR 1191）；这不限制进程内存或 work directory 内容，也不构成 OS filesystem sandbox。
`SkillsExecConfig.work_dir` 是绝对本地 scratch 父目录；每次 Skill 调用持有独立 `TempDir` cwd，正常结束时尝试删除，Windows 句柄释放延迟时通过父目录 capability 有限重试，配置根经 `DirectoryCapability` 检查 reparse point（ADR 1179）。每次新调用会通过 capability 有界检查至多 4096 个直接子项，最多回收 16 个超过最大 Skill 执行期限加 60 秒的 Haven 命名目录；owner 文件的跨进程锁保护仍活动的调用，重解析点、过新、无法确认和超过扫描上限的项目都会保留（ADR 1180）。崩溃残留在后续 Skill 调用时机会性回收，不在 UI 启动时全盘扫描；相对 cwd 输出不会跨调用或 Skill 共享。cwd 仅拥有临时文件生命周期，不是 OS 文件系统权限边界。
`haven-platform::filesystem::is_link_or_reparse_point` 统一检查单个 metadata 是否为 symlink 或 Windows
reparse point；`open_read_only` 在 Windows 禁止新写入/删除句柄，`same_file_identity` 比较已打开句柄的文件身份，
供调用方关闭路径解析与后续读取之间的竞态；这些 helper 不解析路径或制定允许根目录。`haven-common::path` 统一提供纯 lexical path equality 与
equal-or-child 比较，不访问文件系统或 canonicalize 路径（ADR 0863）。App 上传/清理、Tools 资产登记和
Tools 安全沙箱与 Memory 历史附件预览读取分别保留各自的路径遍历、canonicalization、根目录策略及 fail-closed 处理。
Memory 只会从受信的 `default_uploads_root()` 与 `default_generated_media_root()` 根目录重新读取预览 bytes；
它不创建、不登记或清理这些资产（ADR 0197、0403、0858、0863）。

`haven-common::usage` 是跨层用量值的唯一契约 owner：`CacheAccounting`、`LlmCallKind` 与
`CacheDiagnostics` 由 LLM adapter 填充，Agent 原样透传，App event 使用同一 Rust 类型，Memory 只在 SQLite
字符串列边界编码/解码。缓存策略、结果和用量来源是闭合枚举；界面和生成的 command DTO 不再把诊断当作
任意 JSON。provider 名称仍是开放字符串，因为它标识配置中的 provider，而非协议枚举（ADR 0852）。

`haven-common::json` 唯一拥有递归 JSON 对象键排序和 canonical bytes 编码；数组顺序保留。Tools 授权确认输入
hash 与 LLM schema/cache identity 复用相同编码，哈希域和 provider 请求/cache key 语义仍由各自领域拥有（ADR 0862）。

`haven-common::bounded_bytes::BoundedBytes` 持有跨网络适配器共享的响应体字节不变量：Content-Length 超限预拒绝、
有界预分配与逐块追加前的容量检查。LLM 与 MCP transport 各自保留流读取、取消、deadline、文本/JSON 解码及协议错误映射；
Common 不拥有网络 I/O 或这些传输生命周期（ADR 0855）。

`haven-mcp` 内部按职责分为 `protocol.rs`（MCP/JSON-RPC DTO 与内容归一化）、
`transport.rs`（stdio、Streamable HTTP、SSE 和进程边界）、`client.rs`（单服务器连接、
限流、重连与健康监控）和 `manager.rs`（多服务器 reconcile 与 LLM caller 适配）；
`lib.rs` 只保留模块声明和公共导出，`sse.rs` 保留为 SSE parser。
连接固定使用当前 MCP `server/discover` 无状态握手，HTTP 通知只通过 `subscriptions/listen`
请求响应流传递；不支持旧 `initialize`、协议 session header 或独立 HTTP GET SSE listener。

`haven-tools` 的工具核心按稳定边界分为 `tool_contract.rs`（Tool、ToolResult、typed
operation 与执行策略）、`registry.rs`（全局注册表、SessionToolOverlay、版本快照与 probe）和
`security.rs`（内部 AuthorizationEngine、跨 crate `AuthorizationPort`、权限继承、disabled operation、路径沙箱与本机安全矩阵）。
在这组稳定模块之上，`OperationRegistry` 持有已安装、deferred 与 session operation；
`OperationCatalog` 是模型可见投影；`AuthorizedExecutor` 做熔断、启用检查、校验、执行和结果分类，
并把经幂等性与 retryability 筛选的工具失败交给 `haven-common::retry::RecoveryPolicy` 决定退避与 attempt budget（ADR 0447）。
crate-private `ToolAuthorizationRequestResolver` 负责从 live session lookup 或 turn snapshot 生成同一 typed
`AuthorizationRequest`；未命中工具的保守 fallback 也只有一份。它不作 allow/deny/confirm 决定，
该决定仍由调用方在 `execute_tool` 之前交给唯一的授权 owner，不在工具 future 里阻塞；跨 crate 调用只依赖 `AuthorizationPort`，具体 `AuthorizationEngine` 不进入默认生产 API（ADR 0893）。
生产工具执行必须携带 `AuthorizationPort` 为该请求签发的 opaque execution proof：auto-approval proof 绑定 session、tool、capability、canonical input、opaque target binding、operation policy、policy revision 与短期限；确认 receipt 经受信交互 owner 校验后才可换成执行 proof，receipt 本身不可直接执行。`AuthorizedExecutor` 在每次真实尝试（包括 retry）前由 AuthorizationEngine 复核 proof 与当前禁用项、deny、sandbox、network 边界。无 proof 的原始执行入口仅存在于测试/test-support 构建，不进入默认生产 API。MCP target binding 由 manager 实例和 live client 生命周期生成，不包含 server URL 或凭据；MCP manager 在路由前再次比较并固定所选 client，server/client 替换后旧 proof 不会被重定向。
`ExecutionWorkspace` 是 Tools 授权 owner 持有的进程级执行根快照，不是 renderer 的导航 workspace。`SecurityConfig.execution_root` 持久化用户选择的可用本地目录；未设置时启动发现仓库根。设置应用时由授权 owner 规范化并打开 capability，再递增 workspace generation 与 policy revision，使旧 proof 失效；每个 attempt 从 proof 捕获同一 workspace snapshot，Files 相对路径解析、默认 Shell `cwd` 和 Agent prompt 都读该 owner 的状态（ADR 1170/1171）。配置根在启动时已失效会让 workspace 变为 unavailable，不悄悄回退到另一目录。`WorkspaceWrite` 未配置 `writable_roots` 时，以该执行根作为写入范围；根不可用时，相对文件路径失败，带路径的写操作失败关闭。显式 `writable_roots` 非空时继续作为写入范围；工具 `allowed_paths` 仍是独立限制。Files 的变更操作使用与授权 proof/policy revision 绑定的 `DirectoryCapability`，并以打开的目录句柄执行相对路径操作（ADR 1169）；这收敛 Files 路径竞态，但不是进程级 OS filesystem sandbox。WorkspaceWrite 会拒绝 NetworkAccess::Opaque 的 Shell、Skill 和 MCP capability；切到 FullAccess 后，这些子进程仍能访问系统授予的路径，cwd 只确定启动目录，不限制后续文件或网络访问，MCP server 的远端数据操作也不属于本机 workspace（ADR 1181）。
Skill venv/pip 准备进程与 MCP stdio server 在 spawn 时清空父进程环境，仅保留 `PATH`、`SYSTEMROOT`、`TEMP`、`TMP`（Windows 另有 `COMSPEC`、`PATHEXT`）；MCP profile 显式配置的运行时变量与凭据引用在此基线上追加。Shell 也先清空环境，仅保留运行基线、用户目录/终端/区域设置和显式代理变量，过滤其它应用凭据与运行时覆盖（ADR 1188）。这些环境 allowlist 减少无关凭据的直接传递，不改变子进程同用户身份的文件和网络权限，也不构成 OS sandbox（ADR 1175、1182、1188）。
`OperationSpec` 是运行时
策略和 manifest 的唯一定义，覆盖 builtin operation view、root tool，以及 MCP/Skill adapter；spec 没有
handler。`policy_for` 是一次调用的 `OperationPolicy`，`catalog_policy` 是目录上界。`ToolManifest`、
`ToolPolicy`、`ToolPresentation` 只由 `project_tool_manifest` 投影，保留为 Tauri/UI 的 IPC 形状。`tool_core.rs`
组合 registry 与 authorization。`tool_runtime.rs` 用一份不可变 `PlatformRuntime` 承载模型、媒体、
tool settings、context limits、shell 与 security，热更新整份替换。crate-private
`ToolRuntimeCoordinator` 是 `ToolCore`、`ToolRuntime`、`ToolBuiltins` 的组装 owner，并负责
PlatformRuntime 发布、MCP discovery config/index 更新和 builtin catalog rebuild 的工具侧顺序。
messaging 与 memory recall 是进程服务，在 `wire_startup` 里绑定一次，不放进这份快照；
`admin_operations` 随成功的 catalog rebuild 写入 `BuiltinCatalog`，其实现类型 `AdminOperationSet` 留在 Tools 内部；App 通过 `AdminOperationPort` 查询授权风险并执行请求。`tool_builtins.rs` 组合 MCP/Skills
与具体 builtin provider。MCP、skills、授权、媒体资产、ToolRun 与 live output 由构造时交出的
`ToolServices` 提供；授权、受管资产生命周期、Skill 执行、live-output sink、ToolRun、MCP runtime 状态和 Skills catalog/config 均通过窄 capability ports 暴露。MCP 工具调用另以 LLM 的 `McpToolCaller` 能力提供。Agent/App 的消费端 ports 映射 Tools owner ports；ToolRun 完成接收器在 Tools 内部绑定状态机与恢复协调器，调用方不接触具体 `ToolRunService`（ADR 0895–0899）。`McpManager` 与 `SkillRegistry` 只留在 Tools composition owner 内部（ADR 0903）。组合根仍是 `ApplicationRuntime`，
不另建 `AppRuntime`。`ToolsFacade` 是对外 façade，保留执行与授权入口、session overlay/asset
lease 操作、目录投影、runtime capability 请求和录音转写入口；启动及 runtime/catalog 更新转发给 coordinator。
AppState::new 是唯一 App composition root；ApplicationRuntime 集中持有 app-scoped service 与 task lifecycle，不再增加平行的 AppRuntime facade。`ToolServices` 是 composition-time capability handoff；AppState 在组合期读取一次，并把选定的 capability 显式交给 Agent adapter 和 ApplicationRuntime；两者只保存窄 consumer port，不重新通过 `ToolsFacade` 取整包（ADR 0922、0924）。运行期 catalog 查询和 App startup task 复用已保存的 Skills port。
可变 `ToolRegistry`、承载 installed/deferred/session overlay 的 `OperationRegistry` 和按需加载器属于 Tools 内部实现；跨 crate 调用方只消费已发布的 `ToolCatalogSnapshot`，测试夹具通过非默认 `ToolCatalogTestSupportPort` 准备 installed/session 工具。生产 `ToolsFacade` 不公开任意 session-tool 注册；`SessionToolOverlay` 自己持有 map 锁、预算准入和版本递增，loader 与 resume 不接触其内部 map（ADR 0900）。
Tools 的 builtin 实现、熔断器、live-output hub、桌面输入模拟、SkillRunner、ToolRun 类型实现模块和 JSON 列表预算 helper 都是 crate-private；builtin 模块和 SkillRunner 类型仅在非默认 `test-support` 构建中提供给跨 crate 测试。App 重置工具熔断器只调用 `ToolsFacade::reset_tool_circuits()`，不取得 registry 或调用其内部方法。`OperationSpec` 是公开 `Tool` trait 签名所需的不透明契约类型，从 crate 根隐藏导出；其 operation-view 实现模块保持私有（ADR 0905）。
MCP profile 的唯一配置 owner 是 Common 的 `ConfigService`。Tools 的 loader、MCP/tool catalog 与 AdminServices 通过 `McpServerConfigSource` 读取它的当前快照；App 命令直接读取同一 owner。Tools 不再维护需由 startup、reload 和每次配置写入手工同步的 profile map；内部 `McpManager` 仍拥有活动连接、client profile 比较和发现结果，这些是运行时状态，不是配置副本（ADR 0901）。跨层只通过 `McpRuntimePort` 获取状态快照、refresh plan、status stream 与生命周期能力，不再把 manager handle 交给 App（ADR 0903）。
能力判断由 tools crate 唯一构造的 crate-private `ToolCapabilitySnapshot` 收口：prompt runtime、
媒体 operation catalog、TTS/STT 与录音 gate 使用同一 typed 能力值，搜索优先级由它统一投影。
给 Agent prompt 与 builtin registration 使用的 `RuntimeCapabilities` 是从该快照派生的窄投影；
`ToolCapabilitySnapshot::project_runtime_capabilities` 只负责此纯转换，不持有第二份能力状态。
媒体能力由单一 resolver owner 推导：分别判断 LLM 路由与专用 STT 后端，再合并录音、OCR、图像生成
和 TTS 等运行时服务可用性；内置工具目录不另行拥有能力解析。
snapshot 每次从当前 `PlatformRuntime`、Router config 与 MCP index 重建；三者没有共同版本钟，故当前不缓存。
该 snapshot 只含能力结果，不暴露 Router、MCP manager、Database 或授权执行 facade（ADR 0331）。
配置侧仍由 app-binary 的 `RuntimeConfigCoordinator` 持有 config apply gate，并准备/发布 Router 与媒体
client；model edit 的完整提交和应用也由它持有。`SettingsRuntimeApplyCoordinator` 从共享 target plan 生成
Settings 有序阶段，驱动命令提供的执行回调，并唯一记录当前 phase、snapshot version、Router published、
restart-required targets 与失败/警告。Security、MCP、context、logging、hotkey 等实际副作用仍由既有 owner
执行；Settings edit/no-op 仍由命令按同一次 `ConfigService::edit` 保留旧 hotkey、snapshot 和 change。该
coordinator 不复制 Router prepare/publish，也不为半失败状态增加 compensation/rollback。审计确认无第二份配置 owner、target mapping 或 Settings payload builder；durable edit 后不增加 compensation/rollback 或显式 retry/restart；失败保留磁盘配置并报告部分 apply failure，重启从磁盘初始化。失败至重启前普通会话和工具调用继续使用各自 runtime owner 当前状态，不等待重启，也不保证所有 consumer 处于同一配置 revision；turn catalog 保持不可变 snapshot，authorization 和执行仍由 live owner 决定（ADR 0372）。`SettingsView` 唯一的 `update_settings` payload builder 使用生成 request `SettingsUpdatePayload`（SettingsPatch DTO）；读取响应使用 `SettingsPayload`（SettingsSnapshot DTO），IPC script 校验 Rust 参数和 UI 直接调用 owner，而完整字段 schema 仍由 Rust 所有（ADR 0372、1298）。Tools admin 的 MCP/Skills/Logging/ToolSettings writers 若修改与 Settings/model 相同的运行时配置域，必须进入统一串行配置边界；不相关域仍由原 owner 负责。实现跟进由组合根创建共享 gate，并经窄 AdminContext 注入 Tools AdminServices；gate 覆盖 durable edit 与 live apply/rebuild，Tauri wrapper 不重复 catalog rebuild。MCP Tauri 管理入口将写入委托给 AdminServices；
add/update/toggle/remove 的持久化、live 连接和 catalog rebuild 均在 AdminServices 的共享 gate 内完成。renderer 的
refresh/reconnect 也先经 `AuthorizationEngine`；typed native request 不进入模型可见 MCP schema。Refresh 授权后仅在
`AdminServices` 内按共享配置 gate 复核版本与完整 diff，reconnect 复核单个已启用 server 与 live client，再执行对应
连接路径。Refresh 的 connect/disconnect status 经 manager channel 投影；直接 reconnect 成功后由 ToolsView 刷新快照，
待确认 reconnect 则由 resolver finalizer 发布当前状态。待确认 refresh 的 partial failure 由 confirmed-only finalizer
筛选本批授权 target 后，经现有 status channel 发布通用 Offline 状态；直接 refresh 继续通过原响应 DTO 返回批次结果，
不会重复发失败事件。连接及其 `catalog_version` 仍由 Tools 内部的 `McpManager` 持有，App 只消费 MCP runtime capability。
应用退出顺序由 `ApplicationRuntime` 负责，coordinator 不增加独立 shutdown 生命周期
（ADR 0333、0337）。
安全矩阵只有 `security.rs` 一个权威来源，五个 Admin surface 由 ADR 0070/0071 定义的 typed
operation 实现。边界见 ADR 0162、ADR 0211、ADR 0212 与 ADR 0213。

`document.rs` 是受管附件的本地派生边界：由 `media.extract` 对 PDF/DOCX/XLSX/PPTX
执行有资源上限的文本/表格抽取，输出带
`document_extract` provenance 的不可信内容；
不执行脚本、不解析外部实体、不向模型暴露宿主路径。抽取失败显式返回不可用结果，不能
把空文本当成成功；`files` 只负责文件系统边界并把 rich path 交给 `media`（ADR 0114、0129）。

`builtin/media.rs` 是 Agent 原生的媒体工具边界：模型通过显式 operation 选择统一的图片、
音频或文档能力。`inspect`、`describe`、`ocr`、`transcribe`、`extract` 使用 `asset_id`；
`record` 先产生受管音频资产，`play`、`speak`、`volume_*` 和 `mute_*` 访问本机音频设备；结果通过
compact `media` reference 回到工具 observation，后续调用可以复用同一个 asset id；持久化
附件仍使用 common 层的 `MediaInput`。窗口截图和录音
进入生成媒体根目录并登记 session lease，`files.read/summary` 对 rich path 也转交到该入口；
媒体派生契约不接受宿主路径或 base64；`media.play.file_path` 是受 `allowed_paths` 校验的
显式本机播放入口（ADR 0089、0123、0133）。媒体派生调用的 usage 作为
`llm_usage.call_kind=media` 与 `call_kind=tool` 单独保留，不污染 Agent 主循环的累计
cache rate（ADR 0124）。原始附件的 MediaPlan 投影同时留下短的 asset→representation
notice，避免模型重复派生或猜测资产是否已经进入上下文（ADR 0129）。

`builtin/lsp.rs` 为活动执行工作区提供 `lsp.find_definition`、`lsp.find_implementations`、
`lsp.find_type_definition`、`lsp.find_references`、`lsp.get_hover`、`lsp.list_document_symbols`、
`lsp.search_workspace_symbols` 与 `lsp.get_diagnostics`。语言服务器通过 `config.toml` 的 `lsp_servers`
配置，按最长文件扩展名后缀选择文档服务器；工作区符号搜索按配置名称选择服务器，不要求调用方提供源码坐标。工具只向服务器打开工作区内的 UTF-8 文档，配置更新后需重启 Haven。服务以受管本机子进程
启动，每次调用创建一次短期 LSP 会话，限制为两个并发服务、单文档 1 MiB、消息 16 MiB、结果
2 MiB。由于服务器是用户配置的可执行程序且可能自行读取文件或联网，操作属于 High risk，
需要工具授权与确认，只允许 Full Access sandbox；Workspace Write、Restricted Network 与
Deny Network 下不会启动进程。Host-process containment 回收进程树，但不被视为文件或网络
sandbox。可执行配置不进入 Settings IPC snapshot（ADR 1340、1341）；Agent 使用和配置示例见
[LSP 使用指南](lsp.md)。

媒体工具的公共契约与实现按职责拆分：`media.rs` 只保留 `MediaTool`、operation/schema、
安全元数据和统一调度；`media_reference.rs` 负责模态分类与模型媒体引用，
`media_asset.rs` 负责普通路径资产登记，`media_content.rs` 负责图片/音频/文档派生，
`media_generation.rs` 负责生成资产，`media_audio.rs` 集中负责统一媒体工具的音频分支和本机
音频设备适配（ADR 0133、0134、0136）。这些模块共同实现一个 `media` 聚合执行边界；模型
目录暴露 `media.inspect`、`media.describe`、`media.run_ocr`、`media.transcribe` 等点号 operation
view，不把聚合根作为模型入口。

Builtin 的模型目录统一按点号 operation view 暴露：例如 `files.read`、`files.get_outline`、
`files.summarize`、`files.search`、`system.get_info`、`system.env.get`、`haven.config.get`
和 `media.inspect`。`files`、`system`、`haven`、`media` 以及其它聚合模块仍作为 native/Tauri
和内部执行边界；模型只接收对应的窄 schema。provider-facing surface 分层维护：`ask`、`notify`、
`shell`、`http`、`system.get_info`、`discover_tools`、`load_skill` 与 `load_mcp` 每轮直接提供；
其中 `discover_tools` 按需提供第二层 root 和第三层 operation 的名称、描述与精确 schema。其它未选中的
builtin operation 保留在 host-owned deferred catalog，由 `discover_tools` 的 `action=load` 按
operation/root 原子加载到当前 session。提示词索引按 `ToolCatalogGroup` 展示：Agent、Session、
Schedule 与 ToolRun 管理操作归入 `workflow`；`ask` 与 `notify` 归入 `interaction`；工具目录、
Skill/MCP 加载器及外部扩展工具归入 `extensions`。Haven、文件系统、Desktop、System、Media、
Network 与 Shell 保持独立能力分类。Skill/MCP 的 `ToolSource` 仍表达执行来源与安全边界，不再作为
能力类别（ADR 1331、1332）。目录可见性只影响模型路由，不改变工具名、授权、确认、sandbox 或执行策略。
记忆操作的稳定模型名与授权 key 仍为 `memory.*`；其 manifest root 合入 `haven`，因此工具设置页将其与
Haven 管理操作显示在同一根节点下，结果仍使用记忆 renderer（ADR 1349）。

内置 operation 的稳定名使用点号能力路径和动词优先的操作段；父级已表达领域时，操作段不重复领域词。
当前稳定名同时用于 `ToolDef`、权限 capability、manifest、提示词、调用方与 renderer。面向模型的单操作
schema 是封闭 object；聚合执行 schema 通过 `oneOf` / `const` 绑定分支。字段缺省、范围、ID 格式与运行时
校验遵循[命名与输入 schema 规范](naming.md#模型可见内置工具名与输入-schema)；schema 仍由具体能力 owner
维护，provider dialect 转换只发生在 LLM adapter 边界。

启用 Skill 只进入紧凑索引，由 `load_skill` 按名称加载为 session-scoped 的 `skill__...`，并在成功加载结果中返回完整 `SKILL.md` 正文作为任务指引；若正文超出有效工具观察预算（全局观察上限与单工具输出上限中较小者）则拒绝激活，避免模型收到截断指令；
MCP 服务器索引保持紧凑，仍由 `load_mcp` 按服务器加载并注册到 `SessionToolOverlay`
（ADR 0127、0131、0137、0145、0148、0816、1331）。Builtin 的 `toolrun.*` 是后台 ToolRun 操作；
ToolCall 表示 ReAct 前台调用路径，没有独立的 `toolcall.*` 工具根。内置 `load` 请求不设固定
operation 数量上限，批次仍受 `context_limits.max_tools_per_request` 的原子准入预算约束。

MCP exposure 按 tool 生效：`direct` 与 `model_only` 进入 provider-native 工具列表；`model_only`
不得成为 CodeMode 的嵌套 capability。`deferred` 经目录显式加载后走 Legacy 原生调用，
`codemode` 只进入受限 CodeMode allowlist，`hidden` 不可发现也不可执行。暴露只控制路由，不替代
授权、sandbox 或网络策略（ADR 1163、1183）。

Builtin 的 Skill 与 tool catalog 请求复用 `builtin/name_list.rs` 的有序非空名称去重原语；
Skill 前缀移除、目录请求展平与 trim 仍由各自领域入口负责（ADR 0865）。

模型可见 observation 对结构化结果优先保留错误、路径、hint 与续读游标；工具定义和失败结果
分别暴露静态/具体 retry safety，仓库会话的 shell/files 相对路径默认对齐 workspace root，
同时保留 Temp sandbox fallback（ADR 0128）。

CI 以 `scripts/check-crate-dependencies.ps1` 对此表执行内部 crate 依赖方向检查；新增或调整
跨 crate 依赖时，必须先更新本表与该检查，并记录 ADR。

---

## 2. 各 crate 职责

### 2.1 `haven-common` —— 共享叶子（数据与工具，无任何内部依赖）

- `config/`：TOML 配置 schema（`AppConfig` / `Settings` / 各子配置）+ `ConfigLoader` 文件边界；
  `ConfigService` 持有版本化 live snapshot、串行 typed patch、原子持久化和无密钥变更通知。
- `types.rs`：跨 crate 的规范类型 —— 实体 ID（`new_id` / `is_canonical_id` / newtype）、`CanonicalMessage` /
  `ContentPart` / `CanonicalToolCall`、`MessageAttachment`、`FollowUp`、`RiskLevel`、
  `CapabilityScope`、`HotkeyMode` / `ShellChoice` 等。`CapabilityScope` 是授权身份的 typed
  owner，提供 capability ancestry；`CanonicalToolCall` 只承载规范化后的参数值，Provider
  wire 参数序列化与完成流解析/截断修复由 `haven-llm::adapters::tool_arguments` 持有。
- `tools.rs`：canonical `ToolDef` 与工具目录元数据；`ToolDef::root_name` 唯一投影 manifest root，或无 manifest 时工具名的第一段，供 Agent prompt 与 Tools catalog 共用（ADR 0859）。
- `media.rs` / `media_detection.rs`：provider-neutral 的 `MediaAsset`、
  `MediaRepresentation`、能力画像、统一文件探测和纯 `MediaPlan` 计划器；只选择安全的
  raw/derived/managed 表示，不执行文件 I/O 或 provider 路由。文件/MIME 探测以
  `media_detection` 为唯一权威实现。
- `prompts.rs`：系统提示词与各专用 prompt 常量（含 `STT_SYSTEM_PROMPT`）。
- `retry.rs`：纯恢复策略模型。调用方提供错误分类、尝试次数和当前单调时钟，获得继续/停止决策与退避时间；不执行 sleep、取消、队列、持久化或任务生命周期。
- `error.rs`：跨边界错误文本的控制字符/空白归一、敏感字段与路径脱敏、长度限制；App `logging.rs` 组合 `log_err` 与 tracing 上下文但直接复用该 sanitizer（ADR 0867）。
- 错误摘要的 `truncate_with_ellipsis` 添加省略号；Agent `prompt.rs::take_prefix_chars` 只截取字符前缀以遵守 prompt 布局预算。二者输出形状不同，保持各自 owner（ADR 0868）。
- `encoding.rs` / `text.rs`：编码解码（UTF-8 → GBK 回退）、XML entity unescape 与文本工具；CLIXML 消息和 Task Scheduler XML 共用 `xml_unescape`（ADR 0864）。
- `json.rs`：递归对象键 canonicalization 与 `canonical_json_bytes`，保留数组顺序；Tools 确认 hash 和 LLM schema/cache identity 共用此编码（ADR 0862）。
- `path.rs`：Windows case-insensitive 与其它平台组件语义下的纯 path equality/ancestor predicates；不解析链接、不 canonicalize 或制定根目录策略（ADR 0863）。

**判定标准**：凡被 ≥2 个 crate 共享、且不依赖任何业务逻辑的纯数据/纯函数，放这里。
OS 句柄和进程生命周期适配不属于该共享契约面，统一归 `haven-platform`。

### 2.2 `haven-llm` —— 模型与媒体能力的唯一实现方

- `haven_llm` crate root 是唯一跨 crate API 路径：稳定 router/client traits、请求 DTO、registry 与媒体投影操作从根导出；`adapters`、`client`、`router`、`types` 等源码模块不直接对下游开放。provider 和协议细节继续留在 LLM 内部。provider wire-style 规范化和能力谓词由 Common 唯一拥有，App 直接从 Common 读取；adapter 相关根出口仅保留实际跨 crate 使用的 `WebSearchMode` / 搜索结果 helper，媒体 builder 内部解析器及含凭据 resolved config 不进入根 API（ADR 1058）。
- `ProviderConfig.provider` 保存厂商身份，`api_style` 保存 wire protocol；设置页预设选择分别投影这两个字段，不能用协议值覆盖预设身份。OpenAI-compatible 只说明聊天请求格式；每个 provider 必须以 `media_capabilities` 的三个布尔值显式声明 STT、TTS、生图 endpoint，默认配置和预设均提供完整值，不再按 provider/host 推导。Common 校验声明与当前 adapter 支持面相容，LLM builder 再按声明分发。缺字段的 TOML 按配置解析失败策略备份并进入只读恢复。
- `adapters/`：按 **`api_style`（线协议）** 分发的 provider 适配与统一 `LlmClient` +
  `with_retry`。LLM error 分类留在 `haven-llm`，attempt/backoff/stop 决策复用
  `haven-common::retry::RecoveryPolicy`（ADR 0447）。能力矩阵见 `adapters/capabilities.rs`：
  - `openai-chat` / `llama.cpp` → OpenAI Chat Completions；embedding 走 `/embeddings`
  - `openai-responses`（OpenAI / xAI `web_search`、Groq GPT-OSS `browser_search`、DeepSeek v4-pro `web_search`）；embedding 仍走 `/v1/embeddings`
  - `xai` → 旧 Chat Completions + Live Search `search_parameters`；新预设使用 Responses API；embedding 走 `/embeddings`
  - `anthropic` → Messages API（`web_search_20260318`，保留 `web_search_tool_result` 与 `encrypted_content`）；无 embedding
  - `gemini` → `generateContent`（`google_search` grounding、queries 与 citations）；embedding 走 `batchEmbedContents`
  - `deepgram` / `assemblyai` → STT only
- `adapters/tool_arguments.rs`：adapter 私有的工具参数 wire 序列化与完成流解析；Common 中的
  `CanonicalToolCall` 不负责 provider JSON 字符串或流截断修复。
- `adapters/openai/prompt_cache_key.rs`：OpenAI Chat 与 Responses 共用 per-adapter prompt cache key
  支持状态、重探测期限和拒绝错误分类；key 生成与 wire 降级仍由各协议 adapter 拥有。
  `adapters::current_epoch_seconds` 统一提供当前 Unix 秒读取，不合并各 provider 的期限策略（ADR 0861）。
- `FinishReason` 是 provider-neutral 结束分类；各 adapter 单独处理协议专有值，共用标签通过
  `FinishReason::parse_provider_value` 归一化。
- `Usage.cache_miss_tokens` 是 adapter 写入的归一化字段；调用方用
  `Usage::effective_cache_miss_tokens()` 取有效值，为零时按 `CacheAccounting` 推导。
- 聊天页「联网搜索」为命名模型级 `off|auto|always`；共享能力层负责解析设置和调用 Provider
  能力分发，具体厂商的模型白名单、请求参数、强制策略和引用格式分别由
  `adapters/{openai/search,dashscope,deepseek,groq,openrouter,xai}.rs` 实现。UI 依据当前 Provider、模型和线协议能力灰显；
  Perplexity Sonar 的 Chat Completions citations 继续作为搜索来源显示；Mistral 的 `web_search`
  仅支持 Conversations/Agents API，当前 Chat Completions 线协议不能承载引用，暂不注入。
- 厂商扩展（DeepSeek `thinking` / Responses `reasoning.effort`、Kimi
  `thinking.type`+`keep` 等）挂在对应 adapter + provider/base_url/model 检测上，
  复用聊天页「思考强度」，不另开线协议。Preset 必须保留具体厂商 identity，不能因它们使用 OpenAI-compatible 或 Responses wire protocol 而折叠为 `openai`。
- `request_descriptor.rs`：crate-private `RequestDescriptor` 显式并列承载逻辑请求用途和所需
  `Capability`；用途到能力的映射复用 `RequestKind::required_capability()`。`RequestKind` 同时是
  当前逻辑请求类型和 public 配置 route key；不增加一一对应的 public `CallPurpose`。Router 将同一
  descriptor 传至 complete、embedding、raw stream 与 aggregated stream 执行边界；health check
  与 native transcription 也在 Router route/permit boundary 从原 `RequestKind` 构造 descriptor。
  health adapter 调用和已选 client 的 native `transcribe` 不再推导 route capability；STT fallback
  以独立 `AudioChat` purpose 重新进入 aggregated route。usage owner 仍由调用方表达（ADR 0319、0329、0339）。
- 模型的两个标识保持独立：`ModelConfig.id` 是 Haven 内部模型配置 ID，供 `RequestPolicy.primary`、
  路由和模型选择 IPC 引用；`ModelConfig.model` 是 Provider 服务模型 ID，映射到协议请求的 `model` 值。
  `switch_model` 的 `modelConfigId` 只接受前者；配置页分别标注并说明二者用途。它们不可合并，因为
  Haven 可用多个内部配置指向同一个 Provider 模型，也可让同一 Provider 模型在不同配置下使用不同参数。
  聊天页模型选择器与设置页共用 `llm.request_policies`，切换只改变全局 Chat 默认路由（ADR 0437、0819）。
- `model_directory.rs`：crate-private `ModelDirectory`，从 Router 的配置 snapshot
  构造 provider client map 与以 `RequestKind` 为原 key 的 primary route；key 唯一保存 route
  purpose，route value 保存 capability 与 model id，执行解析核对 descriptor capability 并 fail closed。它还集中 client/model
  选择、capability profile 和 endpoint/context-window metadata 查询；生产路由同时要求
  所需 `Capability` 与可用凭据，测试注入只跳过凭据过滤。metadata 借用 Router 的单一
  `RouterConfig` snapshot，不复制配置真源。配置/metadata helper 继续接收 `RequestKind` 并
  经 `RouterConfig::route` 校验 route，不调用 provider 或投影 usage/health；`capability_profile`
  只读取已选 adapter 的本地 wire profile。`connection_status` 与 `prewarm_all` 是明确的健康
  probe，会调用 health check 并按既有规则投影 outcome（ADR 0316、0329、0339）。探测遇到
  open circuit 时返回 `circuit_open` 分类并以 debug 记录，因为该路径没有访问 provider；
  用户对错误/暂停会话执行 Continue 时，仅清除所选聊天模型的连续失败熔断门槛，保留
  rate-limit cooldown 与历史调用计数（ADR 0421）。
- `router.rs`：`LlmRouter` 保留配置 snapshot 与请求执行状态，拥有 route/client 选择、
  health/circuit、rate-limit cooldown、semaphore 与 stream rules，并为执行器提供配置
  snapshot 和 health/rate-limit outcome closure。每个 request kind 仍只走唯一 primary；
  同一模型内重试耗尽后直接返回错误，不跨 provider/model 切换缓存命名空间（ADR 0192）。旧 `llm.roles` 仅在
  配置加载时转换，不进入生产路由；`CallExecutor` 执行 complete/embedding，`StreamExecutor`
  只执行 raw stream 建流与 permit 包装；`AggregatedStreamExecutor` 执行聚合流状态机。
  执行器已接收显式 descriptor。public request DTO 继续以 `RequestKind` 表达兼容 route key，且
  descriptor 的 `purpose` 仍是 `RequestKind`；审计确认这是 route purpose 的同一权威值，不需要
  额外 public 类型。health/native transcription 已在路由边界使用 descriptor，无需再传进不拥有
  route 语义的 adapter。`LlmCallKind` usage owner 由 Agent/Tools 调用方显式设置；相同 route 可以
  有不同 owner，因此 Router 不推断也不接收该字段（ADR 0339、0354）。
- `call_executor.rs` / `stream_executor.rs` / `aggregated_stream_executor.rs`：接收 Router
  已解析的 descriptor、model/client 与单份 `RequestExecutionPolicy`，复用 request pipeline 执行 complete/embedding、
  raw stream 建流或聚合流执行，并经 Router 注入的窄 outcome closure 投影健康状态。raw
  `PermitStream` 持有 permit 到 stream
  drop；聚合执行器集中首次 `on_chunk` 交付前重试、规则触发后的 guidance 重试、取消、总 timeout、attempt
  hooks 和最终结果交接。Router 的 permit 覆盖完整聚合执行；health/cooldown 状态仍由 Router
  更新（ADR 0318、0327、0328）。
- `streaming.rs`：只执行单条 provider stream 的创建后消费与聚合，负责 idle timeout、取消、
  stream rule 检查、chunk 顺序与 `LlmResponse` usage/content 累积；逻辑请求的多 attempt 状态机归
  `AggregatedStreamExecutor`（ADR 0328）。
- `request_pipeline.rs`：provider-neutral 的 `RequestExecutionPolicy`/`RetryPolicy`；
  为普通聊天、工具聊天、embedding、raw stream 建流和 aggregated streaming endpoint
  尝试提供同一份重试预算快照与总超时执行语义。Router/执行器共用这些 helper；adapter
  不实现第二套重试。
- `adapters/transport.rs`：所有 provider 共用的 reqwest client、代理/归因与
  认证头、HTTP 状态错误、流式响应头超时和健康检查；不解析 provider payload，
  也不拥有 router 的重试与路由状态（ADR 0024）。
  `ProviderConfig.default_timeout_streaming_secs` 会物化为 `ModelEndpoint.timeout_streaming_secs`，
  是从连接开始到响应 body 完成的 HTTP 总时限；未配置时，
  adapter 仍限制响应头等待，router 的 `max_total_duration_secs` 与
  `stream_idle_timeout_secs` 分别限制逻辑请求总时长和 body chunk 空闲间隔。
- `adapters/stream.rs`：共享 SSE/JSON-lines framing、EOF flush 与空
  `StreamChunk` 基线；不解析 provider payload，也不拥有 HTTP 请求或重试状态
  （ADR 0025）。
- `adapters/embedding.rs`：OpenAI-compatible embedding 的 URL、请求体、响应
  排序/校验和 usage 转换；provider 选择 endpoint，transport 负责通用 HTTP
  错误（ADR 0026）。
- `adapters/web_search.rs`：内置 web search call 的 action 规范化、citation
  结果 DTO 和按 id 去重；provider 只捕获 wire 事件，Agent/UI 消费统一结果
  （ADR 0027）。
- `adapters/{openai/search,dashscope,deepseek,groq,openrouter,xai}.rs`：各自拥有
  搜索模型白名单、wire 参数、强制语义、usage 解释和 citation 投影；
  `adapters/capabilities.rs` 只协调共享 `off|auto|always` 模式与适配器分发
  （ADR 1291）。
- `adapters/provider_features.rs`：vendor 检测、thinking/reasoning 映射、
  echo 判定及 reasoning 长度限制；Chat/Responses adapter 共享同一规则
  （ADR 0028）。
- `stt.rs` / `ocr.rs` / `tts.rs` / `image_gen.rs`：各专用客户端实现 + 统一分发入口
  （`build_stt_client` 等）。
- `media/`：provider-neutral 媒体原语——common 探测器类型、provider content parts、MediaPlan 投影和
  vision adapter。媒体理解、OCR/STT fallback、文档抽取和文生图由 `haven-tools` 的
  `builtin::media` 工具统一编排；图片/音频以内联 `ContentPart` 进入模型，普通文件落盘后
  以受管 `asset_id` 交给 `media`；视频只有在 capability profile 明确支持时才投影为
  `RawVideo`（当前 Gemini 支持，其它 adapter 显式返回不支持）。TTS 由
  `media.speak` 显式触发并在本机播放；Windows 设备适配仍隔离在
  `builtin/media_audio.rs::AudioRuntime`。
- `tts.rs` 的 TTS client 由 `haven-app-binary` 注入 `haven-tools`；它不是媒体工具的
  自动处理分支，因此用户文本不会因为关键词被隐式朗读。
- `registry.rs` / `stream_rules.rs`：模型注册表与可显式配置的流式规则；生产 router 默认不拦截代码块。

**判定标准**：一切「与模型 / 云端 provider 打交道的实现」都在这里；其它 crate 只通过
`LlmRouter` / `*Client` trait 消费，不实现。

### 2.3 `haven-memory` —— 持久化与记忆存储

- `schema.rs`：唯一的当前 SQLite schema、FTS/embedding 维护对象、版本戳和
  初始化编排。数据库 schema 是严格的 reset contract，不在运行时承载历史迁移。
- `repositories/`：会话、消息、步骤、图谱、用量和任务的持久化读写；其中
  `fact_graph.rs` 集中负责 `facts` 写入与图谱不变量，`fact_query.rs`
  负责事实读取、搜索/排序，`fact_maintenance.rs` 负责事实清理、衰减与矛盾
  扫描，`fact_security.rs` 唯一维护敏感信息规则，并供 Rust detector 与批量 purge SQL 共用；
  `embedding_store.rs` 以窄异步 `MemoryEmbeddingStore` 提供嵌入索引生命周期
  的持久化端口，`memory_recall_store.rs` 以 `MemoryRecallStore` 提供异步 typed
  keyword/vector recall、可见事实 hydration、revision 与完整 recall 端口；
  `tool_run_store.rs` 以异步 typed `ToolRunStore` 提供后台 ToolRun 与 completion outbox
  的窄持久化端口，并在 Memory 内调度 SQLite blocking 操作；`facts.rs`
  负责事实类型、谓词归一化策略和稳定 `Database` 外观。消息的
  `media_inputs` 是多模态 canonical 持久化投影；消息返回对象中的 `attachments` 仅是
  ingress/UI DTO。数据库的 `ui_metadata` 只保留 UI 展示与受管资产保留所需的元数据，
  并由受信 host 根目录重建历史预览，不参与 provider 规划或 transcript 恢复（ADR 0197）。
- `embeddings.rs`：向量编码、相似度/ANN/LSH 查询、底层向量读写，以及 episode FTS 查询；关键词与向量检索仍由 `MemoryRetriever` 组合。

schema 初始化不改变 X12：`session_events` 经 `SessionStore` 追加并按
sequence replay，是会话恢复、rollback、交互重建和实时订阅的唯一事件权威；
`messages` / `session_steps` 仍是投影，生产路径没有独立的 ReAct checkpoint 表，
也不把可恢复的 ReAct JSON 写回数据库。`ReActState` 只存在于进程内作为投影
scratch；完整 `events`、interaction、usage、run budget 和多套 cursor 不得写入
数据库快照。`sessions.react_state` 已随 schema v28 删除；旧库按 reset 丢弃，不迁移，
测试 transcript 只投影 `session_events`。
`UserInject` 事件只保存 `MediaInput` 元数据，reset 只替换持久化载体，不成为新的业务真源。

**判定标准**：负责 SQLite 生命周期、记忆数据持久化，以及从受信 media roots 重建历史附件预览；
不负责创建、登记或清理媒体文件。Agent 编排、LLM provider 协议和 UI 展示逻辑不得进入本 crate。

Agent 的 `memory_service.rs` 是 prompt/worker 共用的 typed memory 边界：它集中管理
有界候选、recall、embedding/index 句柄和 prompt-memory cache；向量行的 scope、敏感
过滤、规范化与 keyword 融合仍由 Memory 内部 `MemoryRetriever` 统一负责；
`MemoryRecallStore` 调度 recall SQL 并返回 typed domain results。Tools 的 `MemoryTool` Recall
只调用已绑定的 `MemoryRecallPort`，由 Agent `MemoryService` 复用该 typed recall boundary；它不再通过
`MemoryFactStore` 执行本地 keyword fallback。`MemoryFactStore` 负责事实持久化与事实查询，未装配 recall
capability 时工具显式失败（ADR 0918）。`MemoryEntityKind` 是 embedding 存储的 `fact` / `episode`
闭合词汇；`MemoryRecallKind` 是 `recall_memory` IPC 的闭合集合，包含 `fact`、`episode`、`profile`、
`semantic` 与 `procedural`。后三类按 facts.memory_kind 筛选，不增加第二套事实存储；renderer 的
`all` 是 UI 筛选项，只展开为 fact 与 episode 两次独立查询，不进入后端 domain enum（ADR 0732、1356）。Agent 保留 prompt
查询归一化、embedding provider 调用、prompt 候选合并与预算；Memory 的 `MemoryRetriever` 在 fact 召回时
从关键词/向量种子沿 concept 节点扩展一跳，最多 16 个种子与 80 个候选，跳过通用 user 节点并复用类别、subject 和敏感内容过滤（ADR 1357）。`MemoryEmbeddingStore` 负责
embedding 生命周期读写和 LSH 维护。Agent `MemoryService` 对 Profile、Semantic、Procedural
分别使用 8/8/4 候选上限，共享一次 query embedding 并统一经 `MemoryRecallStore.retrieve`；prompt
最多 15 条 fact 对每类先保留两个候选，再由原有分数补齐（ADR 1358）。Memory `fact_history` 按 system time 保存语义更改与维护删除前的事实快照；只有显式 MemoryTool `history` 查询读取它，search/list/recall、Prompt、FTS 与 embedding 仍只使用当前 facts。`forget`、清空和敏感事实清理同步擦除历史副本；时点结果经 `MemoryFactStore` 的有界快照端口返回（ADR 1359）。模型从 transcript 与 compaction summary 抽取的事实先原子进入最多 500 条的 `fact_proposals` 队列；只有 Memory Center 显式接受后才通过 FactGraph 写入 facts，拒绝与接受后的提案都会删除（ADR 1360）。`memory_index.rs` 保留模型路由、provider 校验、
批处理和维护门控（ADR 0021、0303、0304）。组合根 `AppState` 通过 Memory 的
`MemoryPersistence::open` 打开数据库，并分别取得拥有独立 live-event channel 的 SessionStore、ToolRunStore
与单一 typed `MemoryStores` bundle。bundle 的 store 字段保持私有，App/Agent 通过具名 getter 获取共享 handle，不能拼接不同数据库 owner 的 store；App 与 `MemoryService` 共用由 `fact_store()` 克隆的能力（ADR 0910）。不再从
`MemoryPersistence` 另行构造同一 typed store。在 Router 和 `ContextLimitsConfig` 确定后，将 `MemoryStores`、Router 和配置中的 `embedding_chunk_size` 注入唯一
`MemoryService`，再将该 service 注入 `AgentLayer::build`；该构造返回
`AgentStartup { agent, memory_startup }`。
SQLite 投影行 `EmbeddedText` 只用于 Memory 内部索引实现，并仅在 `test-support` 下随 raw `Database` 暴露；生产 recall 使用 `MemoryHit`，不泄漏 embedding 表行。
Outbox、session enumeration 与 session-event replay 的 page capacity 均由 Memory typed store 固定并持有；Agent runtime 只推进 high-water/cursor，不传入数据库页容量（ADR 0907、0911）。
仅供 raw `Database` 测试使用的 `PartialMessageCheckpoint` 不进入默认生产 API；生产 checkpoint 生命周期经 Agent `PartialStore` 与 Memory `SessionStore` 收口。
`SessionStore` 的普通生产读取与写入经 typed async/cancellable 操作进入 SQLite blocking pool；同步读写 helper 留在 Memory 内部，测试探针仅由 `test-support` 暴露。唯一同步生产例外是 Tauri `RunEvent::Exit` 必须先于 blocking teardown 完成的 `pause_running_sessions`。App 通知标题 cache miss 使用 `get_session_title_or_input` 异步读取，`TauriEmitter` 的单消费者 FIFO 保持事件投影顺序（ADR 0909、0913；退出顺序见 ADR 0287）。
AgentLayer 从同一 service 派生 stores、共享的 `MemoryWorker` 与 `SystemPromptBuilder`；
`MemoryStartup` 持有唯一 `MemoryRuntime`，并由 ApplicationRuntime 接管长期所有权。Prompt-memory
cache、embedding index 与 worker memory capability 仍属于同一服务实例（ADR 0364、0367）。
Memory 的查询缓存、缓存 generation 与数据 revision 仅供 crate 内 repository 协作；下游通过 typed stores 消费记忆能力，不接触缓存维护 API（ADR 0891）。
`SystemPromptBuilder::with_memory_service`
是唯一公开 builder 构造入口；它不从 Database 创建额外的 MemoryService，因此不产生独立的
prompt-memory cache 或 embedding index（ADR 0365）。`MemoryService` 构造并持有共享的
`MemoryFactStore`，`MemoryWorker::load_known_facts` 通过其有界读取端口取得抽取上下文；
blocking 调度、事实有效置信度顺序、敏感事实过滤和 limit 属于 Memory，Agent 仍负责
prompt 行格式、subject 前缀与字段清洗（ADR 0307）。
MemoryTool 的 search/list 使用带显式 limit 的 `MemoryFactStore` 读端口；查询归一化、敏感事实过滤和截断都在该 owner 内完成，跨 subject 列表的顺序是有效置信度优先，而不是时间最近优先（ADR 0921）。
`MemoryFactExtractionStore` 通过同一 `MemoryService` owner 提供 ordinary/summary extraction 的
typed cursor、共享节流时间戳，以及 ordinary transcript projections（ADR 0308、0312）。Agent 负责窗口构造、
LLM 调用、候选解析/清洗和事实写入策略；生产 ordinary extraction 经
`MemoryFactStore::commit_ordinary_extraction` 同事务写入事实与 transcript cursor，summary extraction 经
`commit_summary_extraction` 同事务写入事实与 completion marker。调用方仍决定新事实置信度下限，写入失败时
整批回滚；不带 cursor/marker 的 `persist_inferred_batch_for_test` 仅由 `test-support` 暴露供测试使用（ADR 0309、0912）。
`MemoryFactWrite` 只承载 Agent 已规范化和清洗的候选字段；canonical predicate 的单值/多值语义由
Memory `FactGraph` 按规范谓词判断。调用方只传新事实置信度下限，不构造或覆盖单值策略标记；该 Memory
policy 不从 crate 根导出，Agent 不重复计算（ADR 0919）。`MemoryMaintenanceStore` 还
提供 LLM 维护的 typed persistence ports：矛盾候选读取、门控后按 id demote、predicate count DTO
读取和逐 predicate rewrite（ADR 0311）。Agent 保留模型配置门禁、提示词/上下文、serde 解析、
候选过滤、安全 gate、日志和降级策略；列表读取失败仍跳过对应 LLM pass，矛盾 demote 失败仍
按原策略计 0，predicate rewrite 失败仍 warning 并继续后续提案。每个 store 调用独立进入
blocking pool；多条 rewrite 不合并事务，repository 内单条 rewrite 原有事务不变。确定性维护也
通过 `MemoryMaintenanceStore` 的逐项 typed 操作调度 repository 方法：facts 去重、敏感事实删除、
规则矛盾 keeper、低置信度 flush、孤儿 embedding、orphan extraction/event cursor 与 source ref 清理。
Agent 继续控制步骤顺序、日志、计数和部分失败后的聚合错误；确定性步骤仍是独立 blocking 操作，
没有新增事务。周期路径把 cancellation token 传到确定性 SQLite 操作边界（ADR 0310）。
生产 `MemoryWorker` 的 raw Database 使用现已清零：摘要 episode cursor 与共享节流戳也经
`MemoryFactExtractionStore` 读写。App composition root 创建 typed stores 后注入 `MemoryService`；生产 `MemoryService` 不接收或保留 backing `Database`，测试的 Database-backed 构造仅在 `cfg(test)`。embedding catch-up 与
LSH lagging 检查沿用 `MemoryService` 的 `MemoryEmbeddingStore` 边界（ADR 0383）。
`memory_worker.rs` 只编排事实抽取、durable outbox、维护、提案提交和索引 catch-up；
`MemoryWorker` 是事实抽取和 maintenance pass 的后台执行编排入口。`prompt_context.rs`
在 turn 边界取得一次工具/运行时/记忆快照，`prompt_renderer.rs` 以纯函数渲染 system
message 与 MEMORY fence，不访问 DB、router 或 cache。事实抽取 outbox 以 `kv_store`
marker 持久化，不把 provider 网络调用下沉到 Memory；事实维护的 SQL 清理与矛盾候选
读取由 `fact_maintenance.rs` 负责，`MemoryMaintenanceStore` 提供确定性与 LLM 维护 persistence
操作的异步 typed 边界；maintenance pass 步骤编排、LLM 仲裁、提案门禁与并发控制仍属于 Agent（ADR 0022、0063、0169、0310、0311）。
Compaction summary episode 与首个 pending marker 只由 `MemoryOutboxStore::persist_compaction_summary` 在同一事务写入；提交成功后 Agent 调用 `MemoryWorker::wake_summary_extract` 更新 live outbox。MemoryOutboxStore 执行 durable marker 持久化操作；Agent `MemoryOutbox` 拥有 scanner、调度、retry/ack 决策和生命周期，Worker 不另行创建 summary marker，避免与 episode 分开提交（ADR 0266、0299、0481、0910）。
`fact_security.rs` 将敏感 predicate 关键词、object 前缀和 marker 作为唯一规则源，并生成精确的批量删除 predicate；现有 `facts.rs` detector façade 和 `fact_maintenance.rs` 的单条 DELETE 共用这些规则，避免 SQL 通配符扩大永久删除范围（ADR 0475）。
`MemoryRuntime` 负责 startup cursor/replay、committed-event live consumer 和六小时维护 schedule policy（ADR 0263、0267）。`AgentLayer::build` 仅在组合过程中创建它，并通过 `AgentStartup` 将唯一 `MemoryStartup` 交给 ApplicationRuntime；AgentLayer 只保留同一个 `MemoryWorker` capability。`ApplicationRuntime` 注册 prepare/startup、live consumer 与 maintenance tasks 并负责 cancel/join。`PreparedMemoryRuntime` 按值消费 prepared receiver；`MemoryStartup::prepare_live_consumer` 产生 `MemoryLiveConsumerHandoff`，ApplicationRuntime 只有在 handoff 注册成功后才将 `MemoryReady` 交给 `AgentLayer::start_after_memory_ready`。prepare/replay 失败或取消时 dispatcher 不启动（ADR 0367、0636）。

### 2.4 `haven-input` —— 输入采集与语音生命周期

- 内部 `capture/`：CPAL 采集线程 + 环形缓冲 + 重采样；`vad.rs`：tract ONNX
  语音活动检测（含常驻 worker 线程）。两者是 pipeline 实现，不作为 crate 模块暴露。
- `lib.rs` 的 `InputPipeline`：录音状态机（`start_capture` / `stop_capture` /
  `cancel_capture`）、固定时长 `capture_for`、VAD 判定与自动停止、PCM/WAV 序列化和采集侧错误。
  `update_audio_config` 替换音频参数；`set_ring_buffer_capacity_secs` 设置下一次 capture engine
  spawn 使用的容量，当前 engine 不在线 resize；setter 复用 Common 的 120 秒资源上限并返回错误，
  0 保持默认容量语义（ADR 1057）。
- 内部 `hotkey.rs`：快捷键字符串解析为中性 `KeyCombo`（与平台解耦）。按键与 VAD
  类型跨 crate 只从根模块导出 `KeyCode`、`KeyCombo`、强类型 `KeyModifier` 和
  `VadSignal` / `VadState`；
  bit flags、engine、detector、ring 与 capture handle 不泄漏到消费者。`KeyCombo`
  只能从格式化配置解析，调用方不能提供任意 modifier bitmask（ADR 0926）。

**判定标准**：管「何时/怎么采」——录音生命周期、VAD 和音频产出；**不实现** provider
调用、转写或 fallback。

### 2.5 `haven-agent` —— ReAct 编排与会话执行

从用户消息派生会话时，Agent 只复制活动事件路径中截至该 `UserInject` 的 transcript 前缀，Memory 在单个事务中创建独立 child session 并写入重映射后的投影；父会话不回滚，完整事件树不属于当前存储契约（ADR 1157）。

- `react/`：模块覆盖 `committed_ui`、`context`、`effects`、`event_boundary`、`identity`、`inject`、`loop`、`metrics`、`request_context`、`response_cycle`、`response_policy`、`sidecars`、`state`、`stream_step`、`tool_admission`、`tool_admission_policy`、`tool_batch`、`tool_batch_execute`、`tool_batch_plan`、`tool_batch_policy`、`tool_ports`、`transcript`、`turn`、`turn_end`、`turn_preparation` 与 `usage`，按 `SessionRun → Turn → ToolBatch` 分层。`response_policy` 分类响应，`response_cycle` 执行有限重试；`tool_batch_plan` 固定调用身份，`tool_batch_execute` 管理并发、取消和逐项 durable commit，`tool_batch_policy` 复用 `RecoveryPolicy` 分类失败与重试预算，`tool_batch` 按 assistant 调用顺序更新 canonical transcript。`context` 只收集有界上下文，`RequestContext` 从 durable canonical 构造不可变 provider request，`inject` 经 `apply_transcript_writes` 投影，`turn_preparation` 在请求投影前准备完整工具面并完成上下文维护，`tool_admission` 定义按稳定调用身份执行的准入契约，`tool_admission_policy` 装配确认与授权策略，`turn_end` 组装最终 `EffectBatch`，`event_boundary` 负责事件流完整性、生命周期边界与暂停记忆触发。
- 流式输出由 `stream_step` 产生，`event.rs` 用一个有序队列归并 thought/reasoning 与工具参数预览；provider retry 通过 `agent:stream_reset` 标记新的输出代次，UI 清理对应的 live stream block 和临时调用预览，不修改 durable transcript。工具参数预览按工具索引合并、限长并节流；不完整 JSON 作为文本展示，不参与解析或执行，最终 ToolCall 投影是权威调用。`streamAggregator` 只合并相邻且同身份的文本 chunk，保留交错输出顺序；最终 thought/reasoning 投影仍是丢 chunk 时的权威修复路径。
- **X12 持久化契约**：ReAct 将 live transcript 作为 `SessionCommitted` domain intent 提交给 `SessionStore`；Agent 的运行时 `TranscriptWrite` 转换为 durable `TranscriptRecord`，后者进入 `SessionEvent` payload；Agent 负责 payload 与消息/步骤语义，Memory 将 intent 翻译为物化行。`SessionCommitted` 只公开有界 builder/合并方法，其 event/projection 集合和 projection-intent DTO 留在 Memory 内部，Agent 不能直接拼接存储投影集合（ADR 0915）。Store 在同一 SQLite 事务中先追加 `session_events`，再写入 intent 指定的 `messages` / `session_steps` 投影；投影失败时整笔回滚，事务提交后才使 cache 失效并广播事件。Agent 随后由 `CommittedUiPublisher` 按 `session_events.sequence` 发布 Thought、ToolCall、Observation、Supplement、ingress MediaPlan、Compaction 与 Session 工作文档更新，再更新进程内 canonical。canonical assistant Thought 消息行、durable event 与共享 `step-*` 执行步骤均在同一 `SessionCommitted` 事务中提交；任一投影失败都会回滚整笔事务，UI 只在提交成功后发布。工作文档是另一种非 transcript domain event：每次整体替换追加 `session_working_document_replaced`，活动事件流最新 sequence 表示当前文本；恢复 DTO 与 realtime event 传输当前值及 sequence，UI 只渲染最新值（ADR 1333）。recovery error-partial 仍按下文所述使用独立恢复写端口。流式分片只用 `chunk_seq`；WebSearch、Usage，以及请求准备阶段的 MediaPlan（`event_sequence` 为空）不占用这条 durable 序号。同一 sequence 的并行工具卡按 `(eventSequence, stepId)` 去重。交互请求由 `SessionActor` 命令追加为 domain event，Agent 从活动 `session_events` replay 交互状态；若 Ask `tool_result` 已提交而独立 `interaction_requested` 尚未提交，replay 以稳定 `step_id` 恢复 pending Ask，后续 `UserInject(source=answer)` 或 `interaction_cleared.request_ids` 关闭它（ADR 0440、0996）。`interaction_requested` 与 `interaction_resolved` 使用 `request_id`；durable replay 兼容旧事件中的 `id` / `ids`，新写入只使用 canonical 名称。resume、rollback 和实时重放均从 event sequence 读取，事件流本身承载恢复游标。rollback 的 event cursor 用于 active transcript 投影，event sequence 用于 append-only timeline；`last_msg_at` 是 `messages`、`session_steps` 与 `llm_usage` 共用的物化投影 cutoff，三者由 `SessionStore` 封装且不得互相推导或作为 transcript 真源。多模态输入在 ingress 接受 `MessageAttachment`，但事件/数据库 canonical 投影使用 `MediaAsset → MediaRepresentation → MediaPlan`，事件不保存 inline bytes；OCR/STT 成功追加派生表示且保留 raw asset。合法旁路限于语义受限的 ingress user seed、recovery partial 与终态 ToolRun-result 三个 `SessionStore` 写端口；seed 消息类型由持久化的 `SessionCreation` 元数据决定。interaction lifecycle event 只承载请求状态与 `request_id` 引用，Ask 正文只在 canonical transcript 出现一次。
- **工具调用身份契约**：同一 assistant tool batch 内，`tool_index` 是 provider 调用数组的零基稳定位置，`step_id` 是该调用的持久执行行/卡片身份，二者共同表达 Haven 的执行身份；`tool_call_id` 保留 provider 提供的不透明字符串，逐字写入 `session_steps` 与 ReAct events。provider ID 缺失或同一 batch 重复时，Agent 生成 `call-*` fallback 并持久化，以维持确认、重试和恢复后的 provider history 配对；不得按前缀重写历史 ID。仅流式参数预览使用进程内 `preview-*`。确认恢复必须按完整身份关联，禁止按工具名、参数或 observation 文本猜测；缺失事件流不再从步骤投影重建 ReAct transcript，旧数据按 reset 边界处理。并行工具的每项结果在完成后单独提交并按 durable sequence 发布 UI；canonical history 与 event replay 按 `step_number + tool_index` 排序（ADR 0433、1046）。
- **工具参数验证契约**：执行前只验证，不用 schema default、首个 enum 或类型占位符改写输入；无效参数以包含 `tool_index`、工具名和验证明细的失败 observation 返回给模型，避免改变副作用语义。
- `session/`：`SessionSupervisor` 只负责 registry、并发 admission 和生命周期；其生产构造接收组合根创建的 `SessionStore` 与 `SessionToolPorts`，不暴露 raw `Database` 或 `ToolsFacade`。`AgentLayer::build` 接收 `AgentToolPorts`；`haven-app-binary` composition root 用唯一共享的 `ToolsFacade` 创建 prompt/catalog/execution/authorization/ToolRun/observation/overlay/asset adapters，Agent runtime owners 持有消费端 ports；授权和 ToolRun 生命周期分别经 Agent-owned `ToolAuthorizationPort` 与 `AgentToolRunPort`，adapter 映射到 Tools owner（ADR 0384、0388、0893、0894）。App 的 ToolRun IPC、shutdown 与生命周期事件接线、Skill 子进程执行、live-output sink、MCP runtime 状态和 Skills catalog/config 分别经 App-owned ports；MCP manager 与 SkillRegistry 留在 Tools owner 内部（ADR 0895、0896、0903）。session runner 通过 `ToolExecutionContext` 传递 session、tool、input、cancel 与 step identity；live authorization 仍在执行前判断。`SessionActor` 独占会话级可变状态，并在自己的 loop 中 select 外部 mailbox 命令与 `SessionState::react_run` active future。`SessionState` 持有会话元数据、交互与 ingress/tool-run 队列，以及 Messaging 轮询游标、节拍和标题缓存；active future 独占 run-local `ReActState`，不会跨 session 共享，也不借用整份 `SessionState`。usage 由 `ReActEngine::UsageRuntime` 聚合和写入，stream identity 与 token estimate 由 run-local `ReActState` 持有。跨 session heartbeat 合并仍留在 `MessagingPoller`；`SessionRunEngine` 是 Agent 内部的完整 ReAct run 执行边界；生产 run 唯一经 dispatcher admission 与启动，不暴露直接启动入口（ADR 0917）；ReAct loop 管 run budget、lifecycle 与 EffectBatch 应用，`TurnEngine` 推进单次 turn 并产出 EffectBatch；`dispatcher` / `queues` / `status` / `tool_runner` 只提供各层协作能力。普通新会话、Agent 创建的子 Agent 与 fork 会话共用同一 Session、SessionActor 与 ReAct 流程；`SessionCreation` 按 `creator`、`method` 和 typed `source` 持久记录创建者、方式及来源，Agent 子会话的来源同时表达 parent lineage；SessionStore 另以对称 `session_links` 表达任意两个会话的显式关联。Fork 将有效 transcript 前缀复制到独立 child session，并重映射会话内 `msg-*` / `step-*` 身份；不会改写父会话，也不引入完整事件树。`session.create` 负责模型发起的子会话创建，`agent` 工具负责 peer 发现、消息和协作控制。Messaging registry 仍负责 peer 角色、能力、mailbox 与在线状态（ADR 0442、1157、1324）。
- `layer.rs` + `ingress.rs` / `resume.rs` / `resume_support.rs`：对外入口与 resume 恢复；`resume_support` 只提供确定性的候选合并、悬空工具调用修复和运行时工具选择恢复。
- `session.search_linked_history/read` 只按需查询当前 session 的直接对称关联，并在匹配结果中保留来源 session 身份；跨会话搜索共用总扫描与结果上限，不向每轮 prompt 注入关联历史（ADR 1345）。
- `canonical.rs`：发送前 `sanitize_canonical` 闸门。
- `memory_worker.rs` / `memory_service.rs` / `memory_index.rs` / `prompt_context.rs` / `prompt_renderer.rs` / `prompt.rs` / `compactor.rs` / `rollback.rs` / `rollback_support.rs` / `title.rs` / `event.rs` / `partial.rs`；`memory_service` 统一 typed memory/embedding/cache 边界，`prompt_context` 取得 bounded turn snapshot，`prompt_renderer` 纯渲染 bounded MEMORY fence；`rollback.rs` 编排生命周期与 DB 双时钟，`rollback_support` 只操作 events 和 branch cursor。
- `token_budget.rs`：Agent 的 tokenizer 初始化、provider-visible message/tool/request 估算与前缀/后缀文本 token 截断唯一 owner。`ReActState` 只持有版本作用域的增量 canonical token estimate cache；`compactor.rs` 保留 compaction 专属的区间前缀和、范围选择与 summary 编排，`prompt_renderer.rs` 保留按完整行选择与 MEMORY fence 布局（ADR 0856）。
- `prompt.rs` 对 memory fact 文本使用 `sanitize_fact_prompt_field` 应用固定 256 字符领域预算；底层控制字符替换与长度截取复用 Common `sanitize_prompt_field(input, max_chars)`（ADR 0868）。
- Memory outbox retry 与 `MemoryRuntime` 恢复退避复用 common 纯策略/退避计算；worker 仍拥有 marker、cursor、等待、取消与恢复时序（ADR 0268、0447）。
- `fact_extraction.rs`：事实抽取 DTO、严格模型字段解析、标签清洗、prompt 字段清洗和
  JSON array 提取；数组元素独立校验并跳过错型条目，规范事实谓词由
  `haven-memory::repositories::facts::normalize_predicate` 唯一拥有，Agent extraction
  与 maintenance 直接复用（ADR 0029、0169、0866、1300）。
- 调用 `LlmRouter`、执行 `haven-tools` 工具、写 `haven-memory`、
  通过 `AgentEvent` 对外发事件。

**步数预算（Phase 7/8 / J1）**：`session.max_steps_per_run` 是每次 ReAct run 获得的步数预算，范围为 1–1000；暂停后恢复会启动新 run 并重新获得该预算。可选
`session.max_steps_per_session: Option<u32>`（默认 `None` = 不限）限制整个持久 Session 跨所有 run 可到达的绝对 `step_number`，范围为 1–100000。Settings UI 显示 0 代表不限，并在提交时转换为 `None`；TOML/IPC 中显式的 0 不合法。
`step_number` 从 Agent 到 Memory/IPC 均使用 `u32`；SQLite `INTEGER` 可存储该完整非负范围，读取时拒绝负值。`tool_index` 保持独立的 `i32` 类型。`max_allowed_step_number = min(max_step_number_for_run, max_steps_per_session)`。二者 owner 都是 `SessionConfig`，一个按每次执行计数，一个按持久 Session 生命周期累计；计算由 ReAct run budget 持有（ADR 0220、1062）。

**判定标准**：会话的业务编排中心，不知道也不关心 provider 细节 / 录音硬件细节。

### 2.5.1 多 Agent 协作（Plan A）

同一机器上多个 session 通过内置工具 `agent` + 文件总线协作；**不**单独做通讯 Tab（产品约束：协作过程以工具结果形式出现在对话页）。

```
Parent session                    Child session(s)
     │  agent operation=spawn          │
     │──────────────────────────────►  │  peer_kickoff（低信任 brief）
     │  agent operation=request        │
     │──────────────────────────────►  │  inbox auto-inject / reply
     │  ← reply (in_reply_to_message_id) + receipt│
```

| 层 | 位置 | 职责 |
|---|---|---|
| 工具 | `haven-tools` `builtin/messaging.rs` | 统一工具名 `agent`；`operation=` list / children / history / send / inbox / ack / reply / profile / request / spawn / status / wait / stop / collect；通过 `haven-messaging::MessagingService` 调用 |
| 服务 | `haven-messaging` `messaging_service.rs` | 唯一应用层消息 port：校验 Envelope identity、claim/complete/retry/expiry、request/reply selective wait、receipt 与删除 tombstone 生命周期 |
| 传输 | `haven-messaging` `inbox.rs` | 权威 JSONL file transport：`%APPDATA%/haven/inbox` 的 registry / append mailbox / immutable backlog + cursor / bounded processing batch / bounded audit archive / deleted-session tombstones / lock；本地 Actor 与独立进程共用相同的消息生命周期 |
| 编排 | `haven-agent` `layer::spawn_peer_session` | 先落库 `peer_kickoff` 并 inbox 注册 parent，再 Pending 调度；返回 `queued`（相对 `session.max_concurrent`） |
| 接线 | `haven-app-binary` `app_state` | 将 Agent 提供的 typed `MessagingRuntime` 接入 Tools runtime；消息服务与协作契约由 `haven-messaging` 提供 |
| 运行时 | `react/context.rs` + `react/inject.rs` | `context` 负责每步 heartbeat、通知或每 3 步通过 `MessagingService::claim` poll inbox（receiver、节拍和标题缓存在 `SessionState`，heartbeat 合并仍是进程级）；每个 envelope 保留为独立上下文项，投影 durable 后由 `MessageClaim::complete` ack 并发 receipt；`inject` 经 `apply_transcript_writes` 注入带消毒后的 `message_id`/`in_reply_to_message_id`/`subject`；`InjectSource::CrossSession` |
| 生命周期 | `session/status.rs` | `interrupt_session`/`end_session` 先取消并立即返回控制结果；若 run 仍在收尾，terminal cleanup、partial promote 与 actor 移除延迟到 dispatcher 的 run-exit 边界；终端态继续 BFS 子孙 system notice + 无嵌套 cascade 结束；`type=system` 仅运行时 |
| 信任 / 记忆 | `memory_worker.rs` | 跳过 `peer_kickoff` 与跨会话注入文本的 fact 抽取 |
| UI | 对话页 tool card | `agent` 结构化卡片；自动同伴邮件以 `agent`/`inbox`/`auto` 卡片展示；kickoff 左侧「低信任委托」 |

协议约定：同伴消息 ≠ 用户指令；JSONL envelope 与工具消息行使用 `message_id`（稳定的 `msg-{uuid32}`）、`from_session_id`、`to_session_id`、`reply_to_session_id` 和 `in_reply_to_message_id`；registry 使用 `session_id` 与 `parent_session_id`。工具收件人参数使用 `to_session_id`，生命周期目标使用 `target_session_id`；
`delivery_attempt` 记录 JSONL transport 的 at-least-once 重投次数；批量消息必须走 `send → claim → process → ack`。
显式 `agent.list_inbox` 默认只 claim 不 ack，并返回稳定的 `message_ids`；处理完成后由 `agent.acknowledge(message_ids)` 确认，每次最多 100 个 ID。单批最多扫描 100 行且源数据最多 1 MiB；每 recipient 逻辑 pending 上限为 16 MiB。超限 delivery 明确失败，不覆盖旧邮件。旧 `.processing` 优先于旧 mailbox 提升为 immutable backlog，游标和 active batch end 共存于一个原子 sidecar；只有整批 ack 后才推进 cursor。模型可见结果不包含进程内领取身份；文件 transport 的 processing 状态在重启后按相同 `msg-*` 重投。`MessageClaim` 是未确认批次句柄，不是独占 lease；多个 consumer 可观察相同 ID。旧 transcript 中的 `claim_token` 不再有效，应重新读取 inbox 并用 `message_ids` 确认。`agent.read_history` 为只读恢复入口，最多 100 条。单条历史 JSONL 行超过 1 MiB 时相关读取 fail closed，原 backlog 保留；operator-only `haven-inbox-recovery` 可按 durable cursor 检查并隔离精确的一条超限行，证据文件保留原始 bytes，完整 inbox reset 仍是格式不兼容、损坏状态及超出 quarantine 容量时的恢复方式（ADR 1328）。`agent.get_status/wait/stop/collect` 只允许当前 session 或其后代，
并通过 `MessagingRuntime` 进入真实 `SessionSupervisor` / `SessionActor` 状态机，`stop` 走正常取消与终端清理路径。
同进程和跨进程 session 都通过同一个 JSONL transport 读取消息；`SessionActor` 只保存轮询游标、节拍和标题等运行提示，不持有第二份 inbox。投递、claim、ack、历史和回复查询始终访问同一 transport，因此 Actor 创建、移除或重启不会切换消息来源。Claim 时写入的 bounded archive 是 delivery audit tail，不是 ack 状态；JSONL processing 中未确认的消息在应用进程重启后以相同 `msg-*` 重投；文件 flush 不代表断电级持久性。此决定与验证见 ADR 1045。

显式删除、retention 与批量删除在清 SQLite session 前先将 Messaging identity 写入 `deleted_sessions.json` tombstone，墓碑只留 `session_id` 与可选 `parent_session_id`。写入、注册、投递、claim/ack、历史和消息查找共享 transport lock；已删除 ID 不可由 heartbeat 复活，也不接受新邮件。删除会清理该 session 的 mailbox、backlog、cursor state、processing、bounded audit archive 及所有临时文件，包含已 ack 的 archive 正文。批量删除还会枚举共享 inbox 的 registry、tombstone 与 canonical mailbox/backlog/cursor/processing/archive 文件名，封锁旧版本遗留的 unregistered recipient。普通 `unregister` 仍表示离线而非删除。墓碑不出现在 agent/child 列表，但 descendants traversal 会穿过它；child cap 仍计入累计墓碑身份（ADR 1116、1117）。

子会话默认工作目录仍为
Temp（全局约束）。

### 2.5.2 内置 `system` 工具（机器信息与系统控制）

统一实现：`haven-tools` 的 `builtin/system.rs`。`env` / `registry` / `power` 以及桌面能力仍可
在代码中由聚合实现承载，但模型目录统一暴露 `system.*`、`clipboard.*`、
`input.*`、`window.*` 点号 operation view；`files.*` 与 `media.*` 也遵循同一规则。聚合根
只保留给 native/Tauri 或内部路由，不作为模型可见入口。

| scope | 能力 | 风险 |
|---|---|---|
| `info`（默认） | 只读机器快照；`category=` 细分 | Safe |
| `env` | 环境变量 get/list；`scope=process/user/machine`，list 可用 `prefix` 过滤 | get=Low；list=High |
| `registry` | Windows 注册表 get/list；路径读取与枚举 | Medium |
| `power` | 电源 status / lock / sleep / hibernate | status=Safe；lock/sleep=High；hibernate=Critical |
| `display` | 监视器几何 + DPI/缩放 + 刷新率 | Safe |
| `clipboard` | 剪贴板 read / write / history | read/history=Low；write=Medium |
| `input` | 键鼠 type / key / click / move / scroll | move/scroll=Low；其它=Medium |
| `window` | 窗口 list / foreground / focus / close / screenshot / UI tree / observe / invoke / set_value / toggle / select / wait；截图后由 `media.run_ocr` 执行 OCR | 读/观察=Low；语义操作/focus=Medium；close=High |

`ToolRunService`（`haven-tools/src/tool_run_service.rs`）只拥有脱离当前 turn 持续运行的后台 ToolRun；ToolRun 状态从 `running` 进入 `completed | failed | cancelled`，`kind` 固定为 `background`。ToolRun completion outbox 是其终态 transcript 交付的 durable owner。工具入口统一为 `toolrun`，包含后台调用与 list/inspect/cancel 管理；前台调用仍使用正常 ToolCall 路径。

Schedule 由独立 `ScheduleService` 与 Memory `ScheduleStore` 持有。`schedules` 保存一次性/周期计划，`schedule_runs` 保存每次 occurrence，`schedule_execution_claims` 串行化执行权；状态 `waiting → running → completed | failed | cancelled` 只属于 ScheduleRun。Schedule 可按时间或被监听 ToolRun 的终态触发，随后经常规授权工具执行路径调用目标工具、向来源 session 投递 continuation，或由 SessionCreation owner 创建新 session。Schedule 计划、occurrence 与取消 UI 由 `ScheduleCenter` 管理，不读取或伪装成 ToolRun 行（ADR 1324）。

Memory Store 分别拥有 ToolRun 和 Schedule 的 SQLite blocking 调度、查询与写事务；Tools 分别暴露窄的 ToolRun/Schedule capabilities。App 组合根装配两个 service、命令与事件；Agent 只消费 schedule fired 与 ToolRun completion ports。ToolRun board/history 只读后台 ToolRun，history limit 在 SQLite 查询前应用。ToolRun 重启恢复将旧进程残留的 running ToolRun 标为 failed，不自动重放副作用；ScheduleRun 按自身 owner 恢复并处理过期计划。状态图与 terminal claim 由各领域自己的生命周期类型单点定义。
后台 ToolRun 全量输出日志位于 `tool-run-logs/<toolrun-id>.log`，由 durable ToolRun 引用、历史删除和启动孤儿对账管理（ADR 1172）。前台 Shell 失败日志没有对应 ToolRun row，使用 `shell-<uuid32>.log` 并按 7 天、128 个文件、32 MiB 总量和 1 MiB 单文件上限回收；超限保留 UTF-8 安全尾部。启动与写入前的清理只识别受控普通文件名，原子写入使用 `DirectoryCapability`；两类日志的生命周期不混用（ADR 1174）。
前台工具输出由 `LiveOutputHub` 按 `session_id/step_id` 管理有界实时预览；ToolRun 不保存 shell 命令、解释器或流式 tail。后台调用结果只在 terminal completion 中持久化，并由 Agent 投影到创建它的 `toolrun.start` ToolCall 卡片。

UI 的 `toolRunStore` 以 `tool_run_id` 作为唯一生命周期 registry；layout 的 `activities` 只是 Svelte reactive mirror。`list_tool_runs` 和 lifecycle channels 共用 `mapToolRunPayload`。后台 ToolRun 的 `tool_name` 是必填身份字段，任务中心按目标工具名、状态、结果和关联 session 展示。terminal history 在 SQLite 查询前限制页大小；Agent 先提交 completion transcript，再 ack outbox，重启后可从 durable outbox 重建交付。ToolRun completion 通知只描述后台调用；Schedule occurrence 的结果与通知由 Schedule/Agent 路径表达。

ToolRun 持久写入仍由 `ToolRunStore` 承担；并发准入由 ToolRunService 控制，终态 CAS、完成投递和历史清理在各自 SQLite writer 边界协调。ToolRun restore 将上个进程遗留的 `running` 标记为失败，不自动重放工具副作用。Schedule timer、确认、执行 claim、恢复和取消由 ScheduleService/ScheduleStore 单独持有。
`InteractionRequest`（`haven-agent/src/interaction.rs`）
是 ask、confirm 和 scheduled confirm 的共同生命周期投影。恢复时 Agent 从 `session_events`
replay active interaction events 并重建 Actor 内状态；该状态不写入独立的 `interactions` 表或
ReAct snapshot。交互状态以 `Pending → Resolved | Expired | Cancelled` 表达。

Clipboard 的文本、HTML、图片和文件列表都从 `clipboard` 根工具进入；图片/文件读取先复制到受管媒体
资产并只向模型返回 `asset_id`。`clipboard.list_history` 是进程内有界文本历史；每次查询会采样当前系统剪贴板的
文本（若可读），因此可纳入其它程序刚写入的内容，但不作为后台剪贴板监视器。`media.render` 复用有界文档表示管线按页返回结果，不新增独立的
`audio`、`file_search` 或 HTTP 搜索根工具。

`scope=info` 的 `category`：

| category | 内容 |
|---|---|
| `overview`（默认） | os + user（当前）+ locale + cpu + memory + disks + network_summary |
| `os` | 名称/版本/内核/发行版 id/主机名/架构/产品厂商与型号/uptime/boot |
| `cpu` | 品牌/厂商/频率/物理·逻辑核/总占用 + `per_core` |
| `memory` | total/used/available/free/swap + usage_pct |
| `disk` | 挂载点/文件系统/kind(HDD\|SSD)/容量/占用/可移动/只读 |
| `network` | 网卡名/MAC/IP/MTU/状态/累计收发（受 `max_output_chars` 截断） |
| `user` | 当前用户/域/计算机名/home/temp/cwd + 本机用户列表 |
| `locale` | 时区偏移/本地与 UTC 时间/系统 locale / UI 语言 |
| `all` | 上述全量（网络仍按预算截断） |

实现依赖：`sysinfo` + Windows `windows-sys`（Gdi / Globalization / Power）。电源寿命字段为秒（Win32 `SYSTEM_POWER_STATUS`）。

### 2.5.3 权限 / 确认（AuthorizationPort；内部实现 AuthorizationEngine）

决策顺序（fail-closed）：

1. `NetworkPolicy` / `SandboxMode` / `tool_settings.disabled_operations` / `allowed_paths` → **Blocked**
2. 操作契约的 `Required` 或 `Critical` → 保留强制确认底线
3. 永久拒绝（`SecurityConfig.permissions`，Always）→ **Blocked**
4. 会话拒绝 → **Blocked**
5. 永久允许 / 会话允许 → **AutoApproved**（但不能越过强制确认底线）
6. `PermissionMode`：`Plan`（仅显式只读操作）/ `Default`（编辑、命令和外部效果询问）/
   `AutoEdit`（安全编辑自动执行）/ `Autonomous`（High/Critical 仍询问）
7. 否则 → `RequiresConfirmation`（事件只带后端生成的安全摘要 + `permission_key`）

决策细化：永久拒绝 → 会话拒绝 → 永久允许 → 会话允许。拒绝授权写工具根键（覆盖同工具全部子操作）；允许写精确键。

能力标识：`permission_key(tool, params)` → `tool` / `tool.operation` / `system.power.lock`；授予父键可覆盖子操作。冒号 operation key 不再参与运行时匹配，配置加载发现这类 key 时备份并按重置策略处理。
操作 view 的 `OperationPolicy.permission_key` 是权威身份；契约另外声明 `effect`、`data_sensitivity`、
`network_access` 和执行并发，不能由风险等级、并发属性或前端字段推断。`SecurityConfig` 另外保存
`sandbox_mode`（`read_only` / `workspace_write` / `full_access`，可选 `writable_roots`）与
`network_policy`（`deny` / `ask` / `restricted` / `open`，默认为 `ask`）。ReadOnly 和 WorkspaceWrite
的文件路径边界只作用于声明为 `OperationEffect::WorkspaceWrite` 的本地文件写操作；其它
`ExternalEffect` 不会仅因只读模式或目标位于可写根之外而被拒绝，而是继续按执行边界、网络策略、
操作风险、权限规则和确认流程处理；无法保证只读效果的工具仍可按各自执行边界被拒绝。
WorkspaceWrite 仍会单独拒绝无法约束文件访问的宿主子进程，因为执行工作区与可写根不是 OS 文件沙箱；
受管隔离执行则由其自身边界控制。`deny` / 不可验证
目的地的 `restricted` 仍直接拒绝对应网络能力；`ask` 将符合执行边界的网络操作交给确认流程，
`open` 不添加全局网络限制。Windows 子进程通过 Job Object 回收进程树；受限网络只允许经过
SSRF/DNS 校验并固定地址的 HTTP/MCP 目的地，禁止跨源重定向。`ask` 与 `restricted` 共用可验证
目的地边界，但默认将普通公网请求交给确认流程，不会因为默认配置而静默拒绝。

当前 `ExecutionWorkspaceSnapshot` 携带后端根和进程内 generation；安全设置提供可编辑路径和受限的原生目录选择器，并通过完整 Settings IPC 持久化。选择器只返回经 Tools 验证的本地目录候选，不授予 renderer 文件访问；空值使用启动发现，保存前再次验证，启动时若持久化根已失效则记录错误并 fail closed。新 attempt 捕获工作区快照；已开始的 attempt 保留其旧 generation 并可完成已授权副作用。`writable_roots` 为空时，`workspace_write` 对带路径的内建写操作默认限制到此根；显式 roots 覆盖这一默认根集合。Files 变更已由授权 proof/policy revision 绑定的目录句柄锚定（ADR 1169），但该策略仍不是 OS 级隔离。scratch 生命周期与 Shell/Skill/MCP 子进程的本机文件访问边界仍按独立切片收敛（ADR 1167/1171）。

确认 UI：拒绝 / 仅本次 / 本对话允许此操作 / 更多允许选项；更多选项按“本对话或永久”与“当前操作、功能组、工具”组合展示，永久授权和扩大范围需要二次确认。没有持久会话 owner 的界面直调与定时确认不提供 session scope；后端也会拒绝 renderer 伪造的 session 决定。拒绝菜单同样支持操作、功能组、工具层级。后端只接受当前 capability 的合法父级，不能由 renderer 发明任意权限键。永久授权写入 `config.toml`；会话授权写入 `session_authorization_grants`，由 `SessionStore` 读取并恢复，且通过 `session_id` 外键随会话删除和历史保留清理。普通会话结束只清进程内 map，授权在重新载入会话时恢复；普通 Security 配置 apply 清进程内 map 后也从数据库恢复。永久规则与会话授权可分别查看、撤销和重置；会话逐项撤销按 session+capability 匹配，永久操作不删除会话 grant。回滚 transcript 不改变授权。安全设置子视图按会话列出 allow/deny、capability、target，并显示每类 reset 的准确影响数量。确认收据绑定规范化输入 hash、权限 key、策略 revision、风险和过期时间，执行前再次验证；原始 shell、网络、文件和扩展参数不进入 renderer。普通全量设置保存不拥有权限规则，避免 stale form 清空授权。授权结果另带稳定 `AuthorizationReasonCode`，调用方不得解析错误文案。

### 2.5.4 Admin Surface

模型看到 `haven.diagnostics.*`、`haven.config.*`、`haven.skills.*`、`haven.tools.*`、
`haven.mcp.*` 等点号 operation view，以及独立的 `tool_runs.*`、`schedule.*`。聚合器只负责内部路由，
每个 operation 继续复用子工具自己的严格 schema、风险等级、幂等性、并发资源和
session 归属；因此 `mcp_add` 是 High，而 `mcp_list` 是 Low，二者不会因共用根名
而被压平。

配置写入使用 `ConfigService::apply_patch` 的 typed patch；普通模型路径没有任意
`config_set(path, value)`。诊断结果只提供脱敏、截断后的日志和 session 元数据，不能
返回 API key、完整 prompt、完整命令输出或会话正文。五个 capability root 由各自的
`TypedToolOperation` 实现，provider JSON 只在 `TypedToolAdapter` 边界转换；native Tauri
command 保存同一组 typed request 并调用对应 surface，未注册 broad `haven` dispatcher。
`AdminContext` 只注入 `SessionStore` 与 `MemoryFactStore` 等 capability-scoped typed handles，
不暴露通用数据库 facade；诊断列表和总数由 `SessionStore` 异步端口提供，最近 50 条状态分组、
limit、创建时间倒序和 errors 的 status 过滤顺序保持原样。组合根将 `MemoryFactStore`
同时提供给 `MemoryTool` 和应用 runtime；缺少对应可选 capability 时保留既有 unavailable 行为
（ADR 0306）。
高风险、网络、媒体、文件和跨 session 协作仍保留独立的内部实现边界，以维持各自的确认、
路径、provider 和生命周期边界；模型看到的名称仍遵循点号 view 契约。
`haven.mcp.connect` 每次都会尝试连接，因此其 `Opaque` 网络分类由 `OperationContract` 同时供模型 operation view 与 native typed request 使用；`deny` 与 `restricted` 均在连接执行前拒绝该边界（ADR 0495）。其它 MCP 管理操作不继承该分类，按各自参数或运行状态定义。

### 2.6 `haven-app-binary` —— 组合根 + 宿主边界（Tauri）

- `runtime.rs`：`ApplicationRuntime` 是应用级生命周期 owner，集中持有 `AppServices` 域能力、
  app-scoped task handles 和根 `CancellationToken`；`shutdown`/`teardown` 统一输入、
  session、ToolRun、MCP 与 bootstrap worker 的停止顺序。领域 worker 仍由所属 crate
  释放，但必须接收 runtime 子 token 或响应领域 shutdown。
- `ApplicationRuntime` 长期持有 Agent 构造结果交接的 `MemoryStartup`，并注册/join prepare、
  live consumer 与周期 maintenance task；prepare/replay 完成且 live task 注册后才获得 typed
  `MemoryReady` 并开放 dispatcher。周期策略仍归 MemoryRuntime，手动 maintenance 命令仍调用
  Agent 的单次 worker pass，shutdown 先停 worker、后按既有顺序 join app tasks（ADR 0367）。
- `app_state.rs`：装配 `AppState`（runtime / 录音生命周期协调器 / bootstrap 状态 / UI
  confirmation）；命令通过 runtime 稳定句柄消费 db / router / tools / executor /
  agent / pipeline / shell / `config_service` / media clients / stt_client；在组合根创建
  supervisor 专属 `SessionStore` 与唯一 `MemoryService` 并注入 AgentLayer（ADR 0363、0364）。
  录音生命周期实现位于 `recording_lifecycle.rs`，只协调 App voice controls 的 `rec-*` identity
  与 start/stop/cancel 互斥；不接管共享 InputPipeline 上的 timed `media.record`。
  `spawn_background_init` 编排音频预热、MCP/Skills catalog 初始化、延迟 session recovery 与
  readiness；任务由 `ApplicationRuntime` 注册并负责取消/join。
  启动、保留期、上传引用和每日媒体清理 task 只捕获 `SessionStore` typed port，不把 raw
  `Database` 传入后台任务（ADR 0374）。
- `config_runtime.rs`：根据 `ConfigChanged` 生成 runtime apply plan，区分 live consumer 和
  `restart_required` consumer；运行时编排留在组合根，不下沉到 `haven-common`。
- `commands/recording.rs`：拥有录音/转写 Tauri 命令与事件状态机；App 录音 command 和
  Shell handler 共用 `RecordingLifecycleOwner`（实现位于 `recording_lifecycle.rs`）串行 start/stop/cancel，stop/cancel 在下一次
  start 前分离 `rec-*` 并把 `RecordingId` 显式交给转写 finalizer。timed `media.record` 不拥有 UI
  recording ID；voice 命令不会接管没有 app-owned ID 的工具采集。transcript 附件落盘委托给
  `commands/managed_media.rs`，不在录音命令内维护文件清理规则。
- `commands/managed_media.rs`：拥有 App transcript 上传的额度、staging、原子提交、文件名/路径校验，
  以及 `default_uploads_root()` / `default_generated_media_root()` 两根目录和 staging 的清理。Common 统一提供 uploads 根路径，
  App、Memory 与 Tools 不再分别拼接 Temp 子目录。一个 App 内部写锁串行化上传与清理；
  两根媒体目录由 App 的清理 worker 按 `SessionStore` durable refs 对账，并通过 `ManagedAssetLifecyclePort` 读取/修剪 Tools owner 的 lease 与 detached TTL；具体注册表留在 Tools 内部。
  generated-media 清理在 lease/TTL 快照前获取 registry 独占 gate 并持有到 unlink 完成；Tools
  producer 从目标文件创建前持 registry 共享 permit 到 asset 登记完成；`FilesTool` 的 canonical rich path
  若解析到 generated-media 根目录的直接子文件，也在 metadata/revalidation/lease 登记期间持同一 permit，
  外部文件不占用该 gate（ADR 0473）。App 内部上传锁仍负责 uploads
  与 staging 生命周期，不暴露给 Tools；剪贴板批次按单文件持 permit（ADR 0470）。`app_state.rs`
  仍拥有启动/每日调度，session 命令仍在历史删除成功后触发清理；Tools registry 和媒体 producer
  不迁入此模块（ADR 0403、0469、0470）。
- `event_bridge.rs`：`AgentEvent` → 前端 channel 和显式 wire DTO 映射，包含 ToolRun
  生命周期投影与通知副通道。
- `handlers.rs`：`ShellHandler` / `InputEventHandler` 的 Tauri、输入管线和托盘适配；user
  recording lifecycle 通过 `RecordingLifecycleOwner` 与命令共享身份交接，保留 VAD、自动停止
  和托盘图标更新适配。
- `bootstrap.rs`：Tauri 启动与桌面生命周期编排，包括托盘、全局快捷键、单实例、自启、日志和退出；
  创建窗口后调用 `AppState::spawn_background_init`，并在该边界提供 Tauri 事件 emitter。后台
  初始化顺序由 `AppState` 编排，具体长期任务由 `ApplicationRuntime` 持有；此模块不承载领域逻辑。
- `lib.rs`：模块声明、移动端 `run()` 入口和必要的 crate 内导出。
- `commands/*`：全部 Tauri IPC 命令（recording / session / tool_runs / history·memory / model / mcp /
  skills / memory / settings / log）。
- `desktop.rs` / `events.rs` / `autostart.rs`。

聊天 UI 的会话状态 facade 位于 `ui/src/lib/sessionReducer.ts`：它导出稳定的
`SessionReducer` / `reduceSession` API、Observable wrapper 和唯一的 `sessionStateStore`。
状态转换按职责位于 `ui/src/lib/sessionReducer/`：lifecycle、transcript/messages、
interaction、usage、Agent stream，以及共享的 types 和 immutable replay/state helper。
领域模块只接收显式 state/action，不互相导入；facade 保留跨域 resume/clear 组合，
live event、resume、rollback 同步和 reconnect replay 都只通过 typed `SessionAction` 迁移。
`createSessionSelectorStore` 只读订阅同一 writable，按选择值引用门控通知；它不复制 reducer
状态，selector 的最后一个订阅者离开时释放 root subscription（ADR 0322）。
`ui/src/lib/chatSessionController.ts` 负责会话命令的异步编排：
权威 resume reload、interaction 保留、切换与终态会话内存回收、rollback、continue、end/interrupt 和
`submitTranscript` 提交适配；会话 Tauri 命令由 `ui/src/lib/sessionCommands.ts` 单一 invoke owner 持有，
controller 通过受限 command port 接收生命周期方法，并通过 typed dependency 接收 reducer dispatch、
session snapshot、通知/错误报告和页面回调，不持有 Svelte state 或 DOM。
`ui/src/lib/chatEventController.ts` 只组合聊天页的 session/app/agent/usage handler map 并拥有
异步注册/释放生命周期；它通过显式 typed dependencies 连接页面 reducer、错误/ask/stream 清理、
session refresh、hotkey 与 model refresh 回调，不持有 Svelte state 或 DOM。`ui/src/lib/events.ts`
是共享 listener registration 和领域 mapper 的调用入口，`chat*EventHandlers.ts` 继续负责既有
事件到页面状态/副作用的适配（ADR 0315）。所有会话生命周期变化共用
`session:lifecycle` 和 `crates/app-binary/src/events.rs::SessionLifecycleEvent` 的 tagged union；
`type` 区分 `created`、非终态 `updated`、`completed`、`error`、`title_updated` 与 `deleted`。
完成/错误原因与终态状态处于同一 payload，聊天页只在单个终态分支执行一次清理。聊天页、根布局
和记忆视图分别消费这条事件流；唯一前端转换位于 `ui/src/lib/contracts/session.ts` 的
`mapSessionEvent`。它把 Rust/Tauri 的 snake_case 字段映射为 handler/reducer 使用的 camelCase，
忽略新增 wire 字段；缺失或类型错误的必需字段会 fail closed 并由 listener 层记录。普通状态更新
只接受 pending/running/paused，waiting reason 只用于 paused；终态不再经第二 channel 副发，
不需要 occurrence identity（ADR 0529）。Rust/App wire DTO 与 reducer 语义不变（ADR 0330）。
ToolRun board 与 lifecycle event 最终共用 App Rust `events.rs::ToolRunEvent` wire DTO。
Tools 通过封闭的 `ToolRunLifecycleEvent` enum 和具名 payload 发出 lifecycle 更新；App
`event_bridge` 映射到 IPC DTO。App 的 `ToolRunKindDto` 与 Tools runtime `ToolRunKind` 当前值相同，但前者
属于 App wire vocabulary，保留类型隔离可避免运行时模型变更隐式改动 Tauri contract（ADR 0530）。
`ui/src/lib/contracts/toolRun.ts::mapToolRunPayload` 是其唯一前端运行时 validator/mapper，
`toolRunStore.refreshToolRuns` 的 command rows 和 `events.ts` 的 ToolRun lifecycle listeners 都调用它。
必需 `id`/`kind` 或已声明字段类型无效时丢弃整行/事件；未知附加字段忽略，未知 status 与 kind
fail closed（ADR 0380）。mapper 不接触 ToolRunService completion outbox；动态
`tool_args` 仍是执行/完成边界上的 JSON 扩展字段，不进入 `ToolRunEvent` UI DTO（ADR 0335）。
录音与转写事件已完成镜像审计：Rust `events.rs` 的命名 DTO 是 wire shape 权威；
`VadStatusEvent` 由 IPC 生成器显式导出，UI `VadStatusPayload` 直接引用该生成类型；
`ui/src/lib/contracts/recording.ts` 只声明其它路由消费的 camelCase DTO，并由唯一的
`mapRecordingEvent` 转换。没有第二份 snake_case wire interface，也没有布局内的字段映射；
`recordingEventListeners` 是这组事件唯一进入该 mapper 的 listener 边界。转换保留既有可选字段省略与
未知附加字段忽略；VAD signal/state 使用 App 闭枚举并从 Rust 生成允许值，畸形或未知值失败关闭。
recording event mapper 也校验 Tauri envelope ID 和 object payload，不合成默认状态（ADR 1018；此前的
开放字符串行为见历史 ADR 0340、0694）。停止原因、错误文本和转写结果的必需字段按 ADR 0962 失败关闭，
不合成默认业务值。
`recordingOverlayController.ts` 是共享 overlay state 与时长 timer 的唯一写 owner，并按 `rec-*`
过滤 stop、error 和 transcription 对当前 overlay 的影响；`+layout.svelte` 仍拥有这些全局 listener、
通知与 voice transcript submission，`Composer.svelte` 只调用 toolbar toggle，`AppShell` 和
`RecordingIndicator` 只负责布局/展示。VAD payload 暂无 session ID，只能按当前 recording 状态门控
（ADR 0472）。
Settings 的完整读取 wire shape 由 `haven_common::config::SettingsSnapshot` 所有，写入使用按域可省略的
`SettingsPatch`；前端不再从多个页面直接读取
`invoke('get_settings')` 的原始结果，所有读取经 `ui/src/lib/settingsCommands.ts::loadSettings` 和
`ui/src/lib/contracts/settings.ts::parseSettingsPayload`。IPC generator 从 Rust Serialize/Serde 元数据生成
`SETTINGS_RESPONSE_SHAPE`，parser 校验所有必需嵌套字段、primitive、数组/映射与闭枚举；patch 只发送已修改的 section，嵌套 nullable 字段可用 `null` 清空。响应可选字段仍可省略，未知附加字段继续保留。
parser 另外校验已知凭据引用为规范 `cred-*` 并检查 Settings 数值域；media timeout 范围由 Rust 与 UI 共同校验（STT 5–600 秒、OCR/TTS 5–300 秒、image_gen 10–600 秒，ADR 1055）。null/非对象继续作为空结果，不完整对象作为无效响应，命令错误原样进入现有 catch（ADR 1020）。
Settings 表单状态仍由 `SettingsView` 持有，`settingsSaveAction` 与 `settingsGuard` 只负责纯 UI 状态，
没有额外的 settings store 或第二个 update serializer。`update_settings` 的唯一 builder 使用生成 request `SettingsUpdatePayload`（SettingsPatch wire DTO）；读取响应使用 `SettingsPayload`（SettingsSnapshot wire DTO）。Rust 参数和 UI 直接调用 owner 由 IPC contract script 校验；前端不复制 Rust nested Settings schema
（ADR 0372）。`hotkey:rebind` 事件已由
`ui/src/lib/contracts/app.ts::mapAppEvent` 唯一映射 `old_binding` / `new_binding` 到 camelCase；
设置页的 `get_log_info`、`read_log_tail`、`get_performance_metrics`、`check_shell_available` 与
`get_api_key_status` 读取统一经 `ui/src/lib/diagnosticsCommands.ts`；`contracts/settings.ts` 的既有
parsers 继续校验 ADR 0007 的命名响应 DTO，并作为日志、shell 与 API-key 响应的唯一 validator/mapper；
metrics 响应保持开放以保留动态诊断字段。
Chat toolbar 的 `switch_model`、`set_reasoning_effort` 与 `set_web_search` 统一经
`ui/src/lib/chatModelCommands.ts` 发出；`chatModelOperations` 拥有用户操作后的 toolbar 状态、错误与刷新抑制，
`chatModelSync` 只从 Settings 派生配置视图并在必要时用相同 adapter 规范化旧 web-search 值，避免多个模块各自
成为同一命令的 invoke owner（ADR 0836）。
`performanceMetrics.ts` 继续拥有 renderer 计数 provider（ADR 0370）。app-shell 事件的批量
`appEventListeners` 与单条 `registerAppListener` 共用 `mapAppEvent` adapter；ToolsView 的 MCP/Skills
刷新监听也经过该入口，布局只拥有 MCP 通知副作用，Skills 不再保留空 listener。Rust MCP status 使用
serde 外部标记 enum，MCP status 只接受当前 Rust DTO variants（ADR 0380）；明确投影的 hotkey/interaction 字段仍只输出
已知 camelCase DTO 字段。布局通知与 ToolsView 刷新是不同副作用，不做 event dedup。`SessionResumeResponse`
中的 interactions 仍由原 session resume normalizer 处理。Agent wire DTO 仍由 Rust `events.rs` 定义；
`contracts/agent.ts::mapAgentEvent` 是唯一 runtime validator/mapper，删除重复的 TS snake_case wire
interfaces，忽略未知附加字段；工具 outcome、retry、idempotency 与 operation scope 必须匹配当前值集，动态扩展值仍只在显式字段保留（ADR 0380）。`agentEventListeners` 对 malformed
payload 记录不含 payload 的 warning 并丢弃；聊天页与布局分别拥有各自的 UI 副作用订阅，共用同一
session reducer，通知、usage fallback 与 media plan 双副作用保持原 owner（ADR 0347）。`SessionCompleted` /
`SessionError` 经 `TauriEmitter` 各只投影为一个 `session:lifecycle` terminal variant；`occurrence_id` 和
配对副发已删除。ReAct Fatal 经项目 dispatcher 运行时仍只由 SessionSupervisor 发布终态错误；dispatcher
专用入口过滤 ReAct 的重复 `AgentEvent::SessionError`，bootstrap 将 typed `SessionSupervisorEvent::SessionError`
重新排入同一 `BufferedEmitter`，由 `TauriEmitter` 按队列顺序投影，并保留标题缓存和桌面通知（ADR 0511、0529）。
Memory command 的 repository `Fact` 仅在 App command mapper 内映射到 `MemoryFactResponse`；该 DTO 与
`MemoryFactSourceRef` 是 renderer 的 Rust wire authority，字段由生成的 `generatedCommands.ts` 导出，
UI 使用生成类型别名（ADR 0357、0529）；UI parser 同时按 Rust DTO 生成的 shape 校验事实与 recall 响应的
完整必需字段、字段类型和闭合来源枚举，并检查 canonical `fact-*` / `msg-*` 身份、confidence/durability
范围及 `mention_count` 的 JavaScript 安全整数域（ADR 1021）。存在的 `source_ref` 必须引用规范 `msg-*` 身份，写入拒绝空或
非规范的 `message_id`，读取时忽略没有 message owner 的孤立 snippet。session、ToolRun、recording、settings read、app event 与
agent event contract 已完成对应 mapper/validator 或边界审计（ADR 0330、0335、0340、0341、0346、0347、
0348、0350、0376）；live interaction event 与 resume snake_case DTO 保持各自 mapper。命令 request/response
的静态 TypeScript contract 由 Rust handler/Serde DTO 生成至 `generatedCommands.ts`，不在多份手写定义间
同步字段（ADR 0394）；生成类型不替代运行时校验，event mappers 与动态扩展 payload 仍按各 domain 手工维护。
前端运行时 contract 的纯值谓词由 `ui/src/lib/contracts/valueGuards.ts` 唯一拥有；wire record 字段存在性与
字符串读取由 `wireGuards.ts` 唯一拥有。Agent、App、Session、ToolRun、Memory、录音、settings response、工具结果 JSON、manifest 与
UI reducer 继续拥有各自字段的必填、空值、缺省和动态 shape 策略；共享 guard 不替代 domain mapper（ADR 0860）。
Settings update payload 仍由 SettingsView 的单一 builder 构造。ToolRun board 的
`list_tool_runs`、`list_tool_run_history`、`clear_tool_run_history` 与 `cancel_tool_run` 经
`toolRunCommands.ts`；list rows 复用 `mapToolRunPayload`，cancel request/result 使用命名 TS contract，
`toolRunStore` 不直接 invoke（ADR 0348）。MemoryView 的 list/add/delete/clear/recall 命令经
`memoryCommands.ts`。命令静态 request/response 统一使用 Rust 生成 contract；每个领域仍负责运行时校验、
直接调用编排和安全审计。事件尚无全局 codegen，各事件 mapper 继续按 ADR 逐域维护。
`continue_session`、`interrupt_session`、`end_session` 与 `rollback_session` 由
`sessionCommands.ts` 唯一 invoke；`ChatSessionController` 通过受限 command port 单一编排并保留原
in-flight 锁、错误处理与通知顺序。请求在 `contracts/commands.ts` 使用命名 DTO。`resolve_confirmation` 留在
`+layout.svelte` 的 shell confirmation
入口，因为弹窗必须跨工作区可见；它使用 generated permission enums 表达 effect、scope、target，
并保留本地 `ConfirmationDecision` view 转换与 in-flight guard。没有重复 request
mapper 或绕过 owner 的 UI caller，IPC script 对照 Rust handler 参数、TS DTO 和直接调用边界
（ADR 0371、0837）。
`+page.svelte` 保留 view/scroll 与 dialog/loading/menu 状态、model sync、resume target/auto-restore、
新会话入口及非 chat-event teardown；ask/input 分流、会话启动恢复和滚动/observer 生命周期分别由
`chatAskInteraction`、`chatSessionStartup`、`chatViewController` 拥有，在 mount 时按 listener-ready 顺序
接线并在 destroy 时 dispose。消息状态由 `sessionReducer` 拥有，`sessionUsage.ts` 是当前共享 usage
投影模块；`streamAggregator.ts` 只负责排队后 dispatch chunk action
（ADR 0160、0313、0315、0320、0322）。阶段 8 的命令 contract 生成与聊天编排范围已完成（ADR 0394）；尚未逐域审计的事件运行时校验、授权策略与页面局部状态仍由各自领域按变更和风险持续审查，不是待完成的跨域 codegen/总 controller 阶段，也不据此机械拆页。
`ModelSettings.svelte` 仍拥有命名模型和 Provider CRUD 编排；活跃 discovery command 现统一经过
`modelDiscoveryCommands.ts`（ADR 0368），页面拥有成功目录缓存、各连接并行发现与结果汇总。单一的
`discover_models` handler 为每个连接使用当前设置草稿的 endpoint/auth/proxy；存储凭据只在连接名和配置 Base URL 匹配时读取，不再另有重复的批量发现 handler。有效空目录仍是成功，失败由 promise rejection 表达（ADR 0872）。ToolsView 的
catalog、MCP/Skills 管理、连接和 refresh 命令均通过 `toolsCommands.ts`；命名 request/response DTO 保持
Rust wire snake_case，MCP config 对齐固定 `McpServerConfig`，动态 schema 仍只在 `ToolSchema = unknown` 边界。
ToolsView 继续拥有 optimistic state、通知、失败显示和 snapshot/event refresh 编排；Rust handler 继续拥有
native admin authorization、连接副作用和 status event。`refresh_mcp_servers` 与 `reconnect_mcp_server` 的 handler
会构造 renderer 专用 typed native request，并在连接副作用前调用 `authorize_admin_request`；确认后的 refresh 部分失败经
现有 status channel 投影（[ADR 0369](adr/0369-tools-catalog-command-contract-boundary.md) 后续决定）。MCP/Skill list DTO 保留扩展字段，builtin `ToolManifest` 仍由
`toolManifest.ts` 唯一投影，卡片列表复用同一批解析行（ADR 0369）。

**判定标准**：唯一能同时看到所有 crate 的地方；负责把事件桥到前端、把前端命令调到后端，
不承载业务逻辑。

---

## 3. 易混边界（历史演进遗留，现已收敛）

### 3.1 input、tools 与 llm 的 STT 边界

| | `haven-input` | `haven-llm` |
|---|---|---|
| 角色 | **采集方**：录音 → VAD → PCM/WAV → `RecordingResult` | **provider 适配方**：`LlmClient::transcribe` + `build_stt_client` / `adapter_for` |
| 编排方 | `haven-tools::builtin::media` 的 `MediaTranscriber` 统一专用 STT → LLM fallback | `LlmRouter::transcribe_audio` 只负责选定模型上的 provider-wire/native-to-chat capability fallback；不做 provider/model failover |

用户语音入口和工具资产入口共享同一个 `MediaTranscriber` 策略，不再各自实现转写：应用通过
`ToolsFacade::transcribe_recording` 把采集到的 WAV 交给工具边界。云端 STT（Whisper / Groq /
Gemini / Deepgram / AssemblyAI）与 chat 共用 `adapter_for` 分发；`provider_name = "llm"` 走
`LlmRouter::transcribe_audio`（原生 `transcribe`，否则 multimodal chat 回退）。
MCP STT 仍走独立 `McpSttClient`（依赖 `McpToolCaller`），Tools 在组合 MCP manager 时用私有 wrapper
实现该 LLM-owned 接口。两条录音路径的生命周期仍显式
不同：UI 麦克风是 `voice input`，只提交转写文本并用 `rec-*` 关联事件；`media.record` 是
`recorded media asset`，先登记 WAV 并返回可复用的 `asset_id`，再附带转写结果。`record`
只依赖采集管线，转写不可用时仍可保留资产并返回结构化 capability 状态。

### 3.2 媒体编排归属

媒体理解与生成统一位于 `haven-tools::builtin::media`，通过 `asset_id`、工具 schema、权限和
tool usage 进入 ReAct。`haven-llm::media` 只保留 modality、provider content parts、
MediaPlan 投影和 vision 等 provider-neutral 原语；`haven-agent` ingress 只负责持久化原始输入。
本次破坏性收敛删除旧的 `MediaGateway` 与隐式 eager preprocessing，详见 ADR 0130。

工具的一次性图片理解由共享 `MediaTool` 编排，并调用
`LlmRouter::analyze_image` 这一 provider-wire adapter；后者只负责将已经读取的
bytes 规范化为一次 vision 请求，不负责 asset lookup、工具权限、生命周期或跨 provider
fallback。`media` 的音频同样由 `MediaTool` 统一处理专用 STT、超时、置信度和
`LlmRouter::transcribe_audio` fallback；媒体派生结果携带 canonical `MediaInput`，不再
以宿主路径作为跨工具引用（ADR 0122、0123、0130、0136）。`files` 只拥有路径安全、读写
和进入注册表的边界；分类与媒体 handoff 分别位于 `file_classification.rs` 和
`file_media_handoff.rs`，rich path 命中 generated-media 根目录时与 cleaner 通过 registry gate
仲裁后再登记；普通路径资产登记位于 `media_asset.rs`。`MediaTool` 的所有
asset/device 分支都通过 common 的 `MediaRepresentationKind` 和 `MediaResult` 外壳投影，
UI、Agent 与 provider 只在各自边界做场景适配。

### 3.3 agent 对 input 的依赖（2026-08-18 清理）

- **改前**：`agent → input` 的唯一理由是重导出历史输入类型（`session.rs`），agent 不调用任何
  input 能力，属于不必要的耦合。
- **改后**：`FollowUp` 下沉到 `haven_common::types`，`agent/src/session/mod.rs` 改为
  `pub use haven_common::types::FollowUp`，删除 `haven-input` 依赖与 `input/src/message.rs`。
  现在 `agent` 与 `input` 不直接依赖；`haven-input` 仅依赖 `haven-common`，Agent 仍依赖
  `haven-common`、`haven-llm`、`haven-memory`、`haven-tools` 与 `haven-messaging`。

### 3.4 依赖方向的守则

- `agent` 不依赖 `app-binary`；provider 不依赖业务 crate；`input` 不依赖 `agent`。
- 两端组件（agent 与 input）的接缝（录音结果 → `submit_transcript` → agent）统一在
  `app-binary` 编排，不通过 crate 依赖互相调用。

---

## 4. 相关文档

- `docs/conventions.md` —— 日志 / 错误 / 通知 / 命令返回规范
- `docs/naming.md` —— 各层命名与跨层 camelCase 边界
- `docs/development-standards.md` —— 架构、契约、安全、测试与变更治理规范
- `docs/ipc-contracts.md` —— Tauri 命令与事件的跨端 DTO 契约目录
- `docs/architecture-refactor-roadmap.md` —— 当前架构阶段、未完成项与验收条件
- `docs/architecture-output-contract-inventory.md` —— 跨 crate 与 Tauri 输出契约清单
- `docs/adr/README.md` —— 架构决策索引；具体 ADR 记录决定、替代方案与影响

---

## 变更历史

历史决策、替代关系和实施细节以 [ADR 索引](adr/README.md) 与具体 ADR 为准；本文件只描述当前架构。
