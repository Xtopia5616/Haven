# Haven 架构降复杂度重构路线图

> 状态：阶段 0–8、§5.8、第二轮 Slice 1–9 与 ADR 1324 均已完成。Session 创建来源/关系、统一 SessionCreation、独立 Schedule owner、`session` / `toolrun` 工具边界已完成。会话执行上下文 Slice 1–5 的实现及 Slice 3/4 验收已完成（ADR 1373–1383）；Slice 2 的执行预设、typed 资源合同、规范 `ResourcePolicy` 与精确边界扩展已完成（ADR 1374、1376–1381）。Shell 系统与工具链读取根配置已完成（ADR 1408）。AppCommand confirmation/admin adapter 已完成（ADR 1302），SessionStore 已完成（ADR 1303–1306），Tools `security.rs` 私有职责拆分已完成（ADR 1307–1311），Agent `tool_runner.rs` 私有职责拆分已完成（ADR 1312），PDF/OpenXML parser 与 App command DTO/process helper 已完成（ADR 1313–1314）。Windows 发布验收仍按 §5.1 跟踪。
> 更新日期：2026-10-27
> 范围：Agent/Session、Memory、Tools、LLM、App IPC 与 UI

本文件只记录当前阶段、仍未完成的工作和验收条件。历史实现切片与决定见对应 ADR；提交历史用于追踪实现过程。

## 1. 目标

减少重复的业务状态与跨层所有权，让持久化、运行时、UI 投影和安全决策各有明确 owner。重构允许删除测试版本中的旧契约，但任何数据、配置、IPC 或安全语义变更都必须有可复核的迁移/重置说明。

## 2. 当前架构基线

- `session_events` 是会话 transcript 的恢复与回滚权威；`messages`、`session_steps`、用量记录和 UI 事件是投影或实时通道。ReAct snapshot 已删除。
- 一个 session 的可变运行态由 `SessionActor` 持有；一次运行的 `ReActState` 是 run-local scratch。
- Agent、Tools、Memory、LLM 和 App 之间通过 typed stores、ports 与 composition root 接线；动态 JSON 限于 provider、MCP、Skill、配置扩展和工具参数/结果等明确边界。
- Settings/model 配置应用、后台/定时任务、MemoryRuntime 与 Rust→TypeScript IPC 契约已有各自的 owner 和 ADR。精确职责以[架构文档](architecture.md)、[跨层输出契约清单](architecture-output-contract-inventory.md)及对应 ADR 为准。

## 3. 长期不变量

1. 一个稳定业务事实只有一个权威来源；投影不得反向成为恢复来源。
2. durable event 只有成功提交后才能发布；外部副作用在事务外执行，并通过稳定 identity 处理迟到或重复结果。
3. Rust/数据库/事件字段使用 snake_case；只在跨端边界映射为 camelCase。
4. 授权必须在副作用执行前由后端 owner 校验；UI 不能绕过安全网关。
5. 删除旧入口时，连同无调用的兼容分支、测试和文档入口一起删除，并说明受影响数据如何处理。
6. 每项重构按独立目标审查，跑适用门禁；阶段完成记录在对应 ADR，不在本文件追加逐日实施日志。

## 4. 分阶段状态

### 阶段 0：基线、契约和观测（先行）

**已完成。** 固定工具链、测试隔离、IPC 校验和可复跑的行为/性能基线已建立；验证细节见对应 ADR 与 CI 配置。

### 阶段 1（已完成）：SessionActor 成为唯一热运行态 owner（P0）

**已完成。** SessionActor 持有会话状态与 active run；actor loop 同时推进运行与 mailbox。见 ADR 0214、0382、0390。

### 阶段 2（已完成）：恢复、回滚和事件/投影边界再收口（P0）

**已完成。** `session_events` 是恢复权威；pending input 的持久路由、Ask 恢复、回滚截断和提交后 UI 发布各有明确边界。待投递输入在 durable marker 中保存 answer/steering/follow_up 路由，恢复不重算；显式投递及 turn/工作完成边界见 ADR 1156；见 ADR 0207、0209、0210、0385、0416、0440、0458。

### 阶段 3（已完成）：存储 domain ports 与 typed projection（P1）

**已完成。** Agent 生产路径通过 typed stores/ports 访问会话、记忆和任务数据；跨层输出清单记录保留的动态 JSON 边界。见 ADR 0383、architecture-output-contract-inventory.md。

### 阶段 4（已完成）：Tools capability runtime 与 Agent ports（P1）

**已完成。** 工具执行、目录、授权和服务适配经显式 ports 接入；组合根负责构建 manager-backed adapters。见 ADR 0384、0388。

### 阶段 5（已完成）：Runtime config apply ownership（P1）

**已完成。** 配置写入 gate、apply plan、部分失败与重启恢复语义已有唯一 owner。见 ADR 0351、0372。

### 阶段 6（已完成）：LLM Router 请求对象化（P2）

**已完成。** 请求描述、能力路由、执行器与 usage owner 的契约已审计收口。见 ADR 0327–0329、0354。

### 阶段 7：统一 Job 生命周期与 MemoryRuntime（P2）

**已完成。** MemoryRuntime 由应用管理；历史实现曾由 ToolRun outbox 承担定时依赖恢复和终态 transcript 投影，现已按 ADR 1324 移交独立 Schedule owner。通用 Job executor、统一 ToolRun deadline、owner token/续租和自动 replay 不在当前设计范围。见 ADR 0367、0392、0393、1324。

### 阶段 8：IPC 单源生成与 UI 编排收口（P2）

**已完成其定义范围。** Rust handler/Serde DTO 生成 TypeScript command contracts；聊天 ask/input、启动恢复与滚动/observer 编排分别委托稳定 owner。事件运行时校验与授权策略仍由各领域按变更持续审查，不构成待补的全局 codegen 阶段。见 ADR 0394。

### 阶段 9：Common 边界与性能复核（条件式）

**状态：条件式候选，不是必须完成的最后一阶段。** 已有性能 profile、文件型 SQLite 容量观测及 `SQLITE_FULL` 注入测试；后续 Common 拆分或性能优化只在依赖图、重复 owner 或同负载 profile 提供明确收益证据时立项。桌面发布验收是独立签核门，见 §5.1；已有基线不能代替当前安装包验收。见 ADR 0359–0361、0404。

## 5. 未完成事项

状态按执行性质区分：**Gate** 是发布前签核条件；**Active** 是当前唯一实施切片；**Next** 是已有证据、等待前一切片完成后再进入的目标；**Acceptance Open** 表示实现已提交但指定环境/交互验收尚未签核；**Candidate** 是已记录证据但不在当前顺序中的工作。结构性目标一次只推进一个 Active slice；行数下降、文件变少或 crate 变少不是完成指标。风险用于确定分片和验收，不单独作为暂缓理由。

| 状态 | 当前项 |
|---|---|
| **Completed** | R12 执行设置选择隔离（[ADR 1438](adr/1438-fence-execution-settings-by-user-selection.md)）：目录选择器绑定发起时选择，保存响应与原读取按选择和请求顺序隔离；8 项回归、UI 1547 项测试、Svelte 检查和生产构建通过；真实 WebView 仍归 §5.1。 |
| **Completed** | R04 inbox 注销失败的清理责任（[ADR 1439](adr/1439-retain-failed-inbox-terminal-cleanup.md)）：registry 读取/注销/离线失败保留原 Actor 与重试策略，独立清理及信任撤销继续执行；5 项故障回归、Agent 710 项测试、workspace 全量测试和全目标严格 Clippy 通过。actorless End 保留 post-commit 错误与显式重试合同，不扩展容量或跨重启保证。 |
| **Completed** | workspace 全目标测试 Clippy 基线（[ADR 1437](adr/1437-restore-workspace-all-targets-clippy-baseline.md)）：独立整理 Common/Messaging/Memory/LLM 测试代码的 10 项既有告警；workspace 全目标严格 Clippy、最终工作树全量 Rust 测试及格式/文档检查通过，生产行为与测试边界保持不变。 |
| **Completed** | R04 后代结束失败重试（[ADR 1436](adr/1436-retain-failed-descendant-terminal-cleanup.md)）：后代枚举/End 失败保留父 Actor、后代提示及原 cleanup 策略，普通失败退避后重试；8 项故障矩阵、workspace 测试和严格 Clippy、Agent 全目标严格 Clippy 通过。不扩展跨重启恢复或 backlog 容量合同。 |
| **Completed** | R12 中断等待隔离（[ADR 1435](adr/1435-scope-interrupt-pending-to-session.md)）：中断准入与等待状态按会话持有，当前 Composer 只显示自身状态；不同会话独立中断，旧请求结束不清除新请求等待，成功通知按选择代次隔离。真实 WebView 交互仍归 §5.1。 |
| **Completed** | R12 结束失败的选择隔离（[ADR 1434](adr/1434-fence-end-session-failure-by-user-selection.md)）：失败恢复限定原选择版本和会话身份，保护新草稿、切走再切回及终态清除后的意图；真实 created/list owner 回归与 UI 1536 项测试、检查和构建通过。 |
| **Completed** | Tools 严格 Clippy 基线（[ADR 1433](adr/1433-restore-tools-strict-clippy-baseline.md)）：收窄历史计数方法并整理测试 lint；Tools 全目标及 Agent 含依赖 strict Clippy 通过，Tools 1021 项测试通过，16 项原有忽略项保持不变。 |
| **Completed** | R04 终态清理重试工作窗口（[ADR 1432](adr/1432-isolate-terminal-cleanup-retry-worker.md)）：合并唤醒、固定 batch、独立串行 worker 与取消重排队，保留 dispatcher shutdown drain；阻塞准入回归、真实 30 秒退出、Agent 697 项测试及本 crate 全目标 strict Clippy 通过。依赖 Tools 的既有 Clippy 告警已由 ADR 1433 修复并通过含依赖检查；不截断待办 map，不新增跨重启保证，R04 整体容量/恢复目标仍是候选。 |
| **Completed** | R12 派生会话动作结果（[ADR 1431](adr/1431-ui-fork-session-action-results.md)）：将创建成功与后续刷新/切换失败分开，页面按发起时的选择版本和弹窗身份协调后续动作；显式操作刷新接通真实查询失败，后台刷新继续安全处理；UI 1532 项测试、检查与生产构建通过，真实 WebView 交互仍归 §5.1。 |
| **Completed** | R12 继续生成的异步选择隔离（[ADR 1390](adr/1390-fence-continue-reload-by-user-selection.md)）：恢复快照与续写提交均校验发起时的会话和选择版本；重载成功/失败、切换会话、新建及切走再切回均有回归覆盖。R12 其余动作职责仍按源码证据复核，不视为整体完成。 |
| **Completed** | R12 UI 会话操作结果切片（[ADR 1415](adr/1415-ui-chat-session-action-results.md)）：切换、rollback 和消息提交返回具名结果；路由处理 rollback 的通知、草稿、弹窗和刷新，提交成功与列表刷新失败分别表达。快速切换、rollback 重入/旧选择及提交后刷新失败均有回归覆盖。R12 其他会话动作仍按源码证据复核，不视为整体完成。 |
| **Completed** | R05 durable UI 事件补齐与 R12 的恢复/rollback 快照竞态切片（ADR 1386）：聊天页可见期间每 5 秒读取当前会话的轻量 durable cursor；序号跳跃或后端 cursor 前进时，双侧 cursor 稳定后才应用恢复投影。快照水位拒绝旧事件，rollback/continue 会使在途旧快照失效。自动化门禁和持续聚焦丢事件的隔离 Windows 验收已通过；恢复与 rollback 并发 UI 场景仍属于 §5.1。R12 其余页面动作职责仍按审查报告作为候选。 |
| **Completed** | R03 模型代次：router、媒体客户端与 MediaConfig 经 App 独占 publisher 原子发布；Agent、Memory、Tools 只读同一代次。每个新 provider/媒体逻辑操作捕获当前最新快照，在途操作保持其起始代次；ReAct 的 run、session 与后台任务不 pin 代次，后续请求重新读取最新快照。目录重建失败时旧目录保持原子且状态 reason 指明已发布版本。见 [ADR 1385](adr/1385-atomic-model-media-generation-publication.md) 与 [ADR 1396](adr/1396-use-latest-model-generation-per-react-request.md)。 |
| **Verification** | R05 UI durable event 补齐与 rollback 快照 fence：稳定 cursor 和快照代次隔离已有实现；新增 cursor 变化期间拒绝不稳定 snapshot、下一轮补齐和重新聚焦立即检查的回归测试，UI 自动测试通过。2026-10-25 隔离 Windows 验收中，Haven 主窗口保持前台可见时由独立进程追加未广播的 durable event，UI 从 cursor 12 补齐至 13；恢复与 rollback 并发的真实 UI 交互仍是 §5.1 未完成项。见 [ADR 1397](adr/1397-reconcile-durable-ui-events-with-rollback-fences.md)。 |
| **Verification** | R09 Schedule handoff：bounded fired queue 满时背压；消费者退出与进程重启时，无 execution claim 的 occurrence 按原 ID 安全重试，已有 claim 的 occurrence 终态标记结果未知且不自动重放；Rust workspace 测试与严格 Clippy 通过。Schedule UI 仍按 ADR 1395 豁免本次 Gate。见 [ADR 1398](adr/1398-schedule-handoff-recovery-outcome-boundary.md)。 |
| **Verification** | R06 录音转写：冻结录音开始时的会话/fresh-session 目标；STT 并发上限为 2；失败源 WAV 保留并可重试，启动清理孤儿文件，同一来源的并发重试去重；UI/Rust 自动门禁通过。真实 Windows 麦克风/VAD、会话切换与重启后重试验收仍开放。见 [ADR 1399](adr/1399-bind-voice-transcription-to-capture-and-retain-failures.md)。 |
| **Acceptance Open** | R08 受管附件资产：上传统一先持有 pending lease，消息接受后绑定到实际 session；提交失败释放本请求租约，durable message reference 独立保护已提交文件（ADR 1400）。自动化覆盖失败释放、成功绑定、上传/GC 竞争、重启空 registry、重复孤立上传、失败批次回滚和路径替换保护。消息提交故障、renderer/应用重启后的真实端到端生命周期仍未验收；不新增持久资产索引。 |
| **Completed** | 新一轮 R02：按 runtime target 显示期望/实际配置版本、结果与净化原因；可在共享 apply gate 下仅重试失败或尚未执行目标，状态见 [ADR 1384](adr/1384-explicit-reapply-of-saved-runtime-configuration.md)。 |
| **Completed** | Slice 2 typed 资源合同（ADR 1374、1376–1381）：Files/HTTP/剪贴板/窗口/注册表与 MCP 远程调用均由可信 adapter 在执行前声明并验证；MCP 执行 proof 绑定连接配置/代次，持久资源授权绑定稳定配置与完整参数摘要。见 [ADR 1373](adr/1373-session-scoped-execution-workspace.md)、[ADR 1374](adr/1374-session-execution-mode-and-resource-contracts.md)、[ADR 1375](adr/1375-resource-scoped-persistent-permissions.md)、[ADR 1376](adr/1376-canonical-resource-policy-and-boundary-extension.md)、[ADR 1377](adr/1377-once-only-http-domain-boundary-extension.md)、[ADR 1378](adr/1378-typed-clipboard-resource-boundary.md)、[ADR 1379](adr/1379-typed-window-resource-boundary.md)、[ADR 1380](adr/1380-typed-registry-resource-boundary.md)、[ADR 1381](adr/1381-bind-mcp-invocations-to-remote-targets.md) 和[设计提案](execution-context-redesign.md)。 |
| **Completed** | Execution-context Slice 3–4 验收完成。Slice 3 在 Windows 11 Insider build 29683 的真实 MXC BaseContainer 上验证 workspace 文件边界、三种网络策略、PowerShell 启动、MXC 后代进程取消/撤权；Slice 4 验证旧 exposure 迁移、三维 MCP 路由和 stale-call 拒绝测试，以及隔离 UI profile 的编辑保存与重启读回。未连接外部 MCP 服务或配置模型；Windows 发布 Gate 仍按 §5.1 开放。证据见[报告](execution-path-capability-audit-2026-10-23.md)、[ADR 1382](adr/1382-execution-path-capability-audit.md) 和 [ADR 1383](adr/1383-split-mcp-availability-catalog-and-adapters.md)。 |
| **Gate** | Windows 发布验收 Open，见 §5.1。 |

### 5.1 Windows 发布验收（Gate / Open）

在隔离的 Windows profile 或 VM 上使用当前构建完成并记录：

- 首启、Settings、会话、工具确认、媒体、后台任务、恢复与回滚的真实 UI 流程；
- 主窗口持续聚焦期间丢弃一次 durable UI 通知，确认游标轮询补齐 transcript；让恢复读取与 rollback 并发，确认旧分支不会覆盖回滚投影；
- 当前安装包的安装、升级、卸载，以及卸载后的用户数据保留行为；
- **本次 Gate 豁免（ADR 1394）**：不要求真实 Windows 物理卷 ENOSPC 测试。现有 SQLite `SQLITE_FULL` 故障注入与错误恢复提示仍按 ADR 0395/0404 保留，但不声称完成物理卷耗尽验收。
- **本次 Gate 豁免（ADR 1395）**：不要求 Schedule 创建、触发和结果展示的真实 UI 验收。此范围豁免不是 Schedule UI 通过证据；后台 ToolRun 等其他项目不受影响。

用最新代码和安装产物复跑适用的 Rust/UI 门禁，并在 ADR 中记录实际构建版本、schema、环境、结果与限制。ADR 0395 中早期 profile 和门禁结果是当时的历史证据，不能当作当前工作树或当前安装包的通过结论。

可以提前准备隔离 profile 和安装器；最终验收必须使用交互生命周期等 IPC/UI 变更完成后的最新构建。此项是发布签核门，不阻塞不影响发布路径的独立模块整理。

**当前 Gate 状态：Open。** Slice 3/4 已在独立开发 profile 和真实 Windows MXC 进程测试中验收；主机为 Windows 11 专业版 Insider Preview `10.0.29683`。Slice 3 的宿主根目录写入/拒绝、`Deny/Restricted/Open` loopback 网络行为、PowerShell 兼容、MXC 后代进程取消与撤权均通过；Slice 4 的隔离 MCP fixture 编辑保存、应用重启读回及本地路由/迁移测试通过。MCP fixture 保持 disabled，没有外部服务器调用。工具链为 Rust 1.99.0、Node 24.21.0、pnpm 12.12.1。Windows NSIS 验收包 `0.1.2` 在隔离 profile 完成 `0.1.0 → 0.1.2` 升级、运行中旧进程升级和卸载：升级后 UI 资源与当前构建 75 个文件完全一致，卸载删除整个安装目录且保留配置/数据库；发现并修复内容哈希资源残留，详见 [ADR 1392](adr/1392-nsis-cleanup-content-hashed-resources.md)。提交 `09123d9` 后重跑 workspace Rust 测试、严格 Clippy、Svelte check 与 UI 测试，均通过（UI 169 文件、1493 项）。当前 0.1.2 包已从隔离 profile 正常启动，并通过唯一 Haven 窗口检查；设置、媒体设置、124 项内置工具、会话空态、历史页和任务页均正常加载且未改动配置。媒体附件选择器打开到隔离 fixture 目录，但 `@oai/sky` 在激活并刷新后重试仍报告 `point (1147, 454) is over msedgewebview2.exe`，未选择或发送文件。Provider 配置指向 `127.0.0.1:34197`，但 fixture 未监听，模型交互、确认通过路径及后台任务仍未验证。当前隔离数据库只读检查为 `quick_check=ok`、sessions/messages/events 均为 0，不能视作恢复验收。Schedule 真实 UI 按 [ADR 1395](adr/1395-waive-schedule-ui-from-current-release-gate.md) 由用户决定从本次 Gate 豁免，不是测试通过。durable UI 通知丢失补齐与 rollback/恢复并发未做真实交互验收。真实 Windows 物理卷 ENOSPC 按 [ADR 1394](adr/1394-waive-physical-enospc-from-current-release-gate.md) 由用户决定从本次 Gate 豁免，不是测试通过；既有 `SQLITE_FULL` 注入与恢复提示证据仍见 ADR 0395/0404。C 盘约余 7.23 GiB、D 盘约余 7.26 GiB；没有填满宿主盘。详细结果见 [ADR 1393](adr/1393-windows-release-gate-recheck.md)、[ADR 1394](adr/1394-waive-physical-enospc-from-current-release-gate.md)、[ADR 1395](adr/1395-waive-schedule-ui-from-current-release-gate.md)、[ADR 1382](adr/1382-execution-path-capability-audit.md)、[ADR 1383](adr/1383-split-mcp-availability-catalog-and-adapters.md) 和 [ADR 1392](adr/1392-nsis-cleanup-content-hashed-resources.md)。

**补充交互检查（2026-10-25，非 Gate 签核）。** 当前开发版在独立 `HAVEN_DATA_DIR` 与 loopback mock Provider 下完成配置保存、模型发现、一次对话提交和响应、切换到新草稿后恢复原 session，以及在 UI 确认后回滚到指定步骤并重放。另行追加的 durable transcript 事件仅用于重启后的恢复观察，未作为同进程实时补齐证据；reasoning 项在执行摘要中折叠显示。持续聚焦时漏通知补齐、恢复与 rollback 并发、真实麦克风转写和消息提交失败后的资产回收仍未验收，因此 Gate 保持 Open。

### 5.2 交互生命周期所有权（行为已收口；恢复覆盖纳入新一轮）

Session、ScheduleRun 与 AppCommand 确认均由各自 owner 和稳定 request identity 路由；Session grant、resolve event/status 与 actor 唤醒顺序有明确的持久化和失败契约。交互 replay 失败保持 fail-closed，pending session 恢复有界并可取消地重试。当前行为契约与逐项验证见 ADR 0423/0424、0507–0522。2026-10-17 源码审计当时发现 AppCommand pending 状态、入队/过期和 resolve continuation 分散于 `app_state.rs`、`commands/mod.rs`、`commands/session.rs`；该 owner 已按 ADR 1302 收拢，且未重定义既有交互契约。

源码复核修正了该边界：grant 写入后，Session confirmation resolution 与 `end_session` 由 `confirmation_resolution_gate` 串行；Actor 先 append `interaction_resolved`，成功后才移除 pending/唤醒 session，append 失败保留 paused pending。ADR 0519 覆盖同一 actor 内重试；actor 退出后的补偿、重试与期限收敛由 [ADR 1327](adr/1327-recover-session-grant-after-resolve-append-failure.md) 明确：已提交 grant 不补偿删除；未载入 actor 的 resolve 返回 stale 且不改 durable state；显式 reopen 按原 request ID 和绝对 deadline 恢复，deadline 已过时立即追加 Expired，不执行该 ToolCall continuation。Startup 只自动载入 `status = pending` 的 session，故 unloaded paused session 不承诺后台自动到期。UI 自动恢复先显示 transcript，等 `reopen_session` 成功后再呈现 pending confirmation；其他历史打开路径同样先 reopen 再读取 resume snapshot。Session grant 的持久 owner 与 Agent/Memory 事务边界不变。Session grant 本身没有基于时间的自动到期策略，当前授权只在明确撤销/重置或所属 session 被删除（含 retention）时失效，见 [ADR 1338](adr/1338-keep-session-grants-without-time-based-expiry.md)。

### 5.3 内部模块与 crate 边界（按证据复核）

本节记录当前源码已确认的结构候选与保留边界；历史决定和验证以对应 ADR 为准。候选依据当前生产职责、调用路径和状态读写方，不沿用旧文档的“高风险暂缓”结论；当前切片顺序见[第二轮方案](refactor-plan-compat-design-audit-2026-10-17.md)，第一轮模块职责切片见[第一轮方案](refactor-plan-source-audit-2026-10-17.md)。

#### 已复核候选与已知限制

| 状态                       | 问题与当前证据                                                                                                                                                                                                                                          | 重开条件与边界                                                                                                                                                                                 |
| -------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **已完成（2026-10-17，ADR 1303–1306）** | `session_events.rs` 曾混合 fork、ingress、pending input、recovery、tool-step、event append/projection、replay、rollback、usage 与 branch marker。 | 统一由 `SessionStore` facade 协调；fork/lifecycle、event model/append/commit/projection、replay/page/active read、rollback/usage compensation/branch marker 均进入私有模块。workspace tests 与 strict Clippy 通过；具体边界和恢复语义见 ADR 1303–1306。 |
| **已完成（2026-10-17）** | layout session/permission 通知读取 `notifyCfg`，ToolRun completion 读取独立 store；Settings 保存只更新后者，造成同进程配置版本分裂。 | layout 现在拥有全类通知唯一运行快照；Settings 成功保存后发布新快照，durable-first partial-apply 时重读已写配置，普通保存失败不更新；revision guard 阻止初始异步读取覆盖晚到的新快照。验收见 ADR 1294。 |
| **已完成（2026-10-17，ADR 1301）** | App composition root 曾将 Tools 提供的 ToolRun/live-output capability 再包成无行为转发 adapter；UI live board 与 session history 各自保有完整 ToolRun payload；命令安全 metadata 只按命令名校验；AgentEvent channel 与 DTO 分别选择。 | App 直接持有窄 capability；UI payload 收敛到单一 map、两个有界 ID 索引；Rust `COMMAND_CONTRACTS` 生成并逐字段校验 TS 与文档 metadata；AgentEvent 在同一 exhaustive projection 中绑定 channel+payload，事件 checker 对照生成 DTO registry。完整门禁见 ADR 1301。 |
| **已完成（2026-10-17，ADR 1302）** | AppCommand pending map、入队/超时、仲裁/授权/continuation 曾分散于 `app_state.rs`、`commands/mod.rs` 与 `commands/session.rs`；admin adapter 也混在通用命令模块。 | 私有 `AppConfirmationOwner` 管理 pending 状态与生命周期；`resolve_confirmation` 保留为跨 owner router；native admin authorization、执行与 UI 收尾移入独立 adapter。Session grant durable/append-failure/reopen 语义仍由既有 Agent owner 保持。App crate 测试、strict Clippy 与 IPC contract 检查见 ADR 1302。 |
| **已完成（ADR 1307–1311）** | `tools/security.rs` 同时实现授权 engine 与有序执行边界检查。 | prompt/矩阵、路径入口、授权 contract、执行 permit、MCP/Skill 两种 process-start adapter、FileRead/FileWrite policy/path helper 与唯一 engine 均进入私有模块；三个授权入口共用按固定优先级执行的硬边界检查。`security.rs` 保持稳定 facade，授权/revoke 状态仍只有一个 owner。Windows workspace tests、strict Clippy 与格式检查通过；并发子进程测试波动和单线程完整门禁结果见 ADR 1311。 |
| **已完成（ADR 1312）** | `agent/session/tool_runner.rs` 同时实现 step persistence、execution routing 与 confirmation/ScheduleRun lifecycle。 | 三类实现已迁入私有子模块；保留唯一 `SessionSupervisor`、既有 step identity 与 confirmation/claim owner。Agent 与 workspace 门禁、格式和 ADR index 检查通过。 |
| **已完成（ADR 1313）** | `tools/document.rs` 同时包含 PDF 与 OpenXML 解析。 | 输入、页游标和输出策略留在 facade；格式解析器与各自限制进入私有模块；11 项相关测试、strict Clippy 和格式检查通过。 |
| **已完成（ADR 1314）/复核后保留** | `app-binary/commands/contracts.rs` 曾混合 command security metadata 与 Memory、Skills、Tools 响应 DTO；`external` 与 `open_skills_dir` 各自 guarded spawn；AppState/bootstrap/cleanup 候选已复核。 | DTO 已归入唯一消费它们的 command owner，IPC Serde 投影不变；detached host process spawn 共用 App command helper，调用方保留 authorization、path/URL policy 与参数；AppState/runtime 和 Tauri bootstrap 保持单一依赖顺序与生命周期 owner，拆 helper 不会形成新 owner。 |

**已关闭候选 — Session end 失败契约：** actorless、驻留 Actor 与运行中 Actor 的取消/清理失败语义、ToolRun claim 竞争及重试边界已按 ADR 0507、0514 收口并联合验证；当前无 Active 工作。只有同类失败/终态不一致再次复发时才重开。

#### 保留边界与较低优先级候选

- **Tools 其余边界保留：** `tool_contract.rs` 继续作为共享执行契约 owner；`builtin/admin.rs` 由 AdminServices 承接副作用，Admin 保留 operation/request/output contract；messaging adapter 继续复用 `haven_messaging`（[ADR 0213](adr/0213-operation-spec-single-policy-source.md)、[0391](adr/0391-admin-services-typed-output-projections.md)、[0396](adr/0396-messaging-domain-crate.md)、[0506](adr/0506-mcp-admin-connection-network-policy.md)）。本轮只拆 `security.rs` 内部职责，不挪 operation policy。
- **工具运行时三个集合各自保留（已复核，暂不合并）：** `ToolRegistry` 是已安装工具的权威注册表，保留注册顺序并拒绝重复名；`DeferredToolCatalog` 保存尚未激活、供发现和按需加载使用的定义；`SessionToolOverlay` 是某个 session 当前可执行的附加工具集合，loader batch 负责幂等、预算准入与 session version。published global/session catalog generation 由 `ToolCatalogVersion` 表达，不在 installed registry 复制一套无生产消费者的 version counter。三者分别表达注册、延迟发现和 session 执行作用域，不能因 lookup/list/definition projection 外观相近而统一成一个通用 Catalog。Provider 定义、校验、manifest 与执行的共享 turn-level immutable view 已由 `ToolCatalogSnapshot` 承担；共同规则若真实重复并导致漂移，再评估窄 helper。ADR 0145/0148 的 provider visibility 与 session 隔离不变（本轮名称对齐见 [ADR 0533](adr/0533-tool-runtime-nomenclature-alignment.md)）。
- **MCP prompt index 边界保留：** `McpServerIndexEntry` 是固定的 Tools→App→Agent prompt 摘要；tool schema/result 保持动态 JSON。仅在摘要新增稳定字段或出现独立消费者时重开（ADR 0516）。
- **Admin 风险 metadata 保留单一来源：** 共用操作由 `OperationContract` 拥有风险级别，parity 门禁覆盖共享 operation 集合；MCP refresh/reconnect 等 native-only 操作保持独立测试。operation surface 或风险策略变化时重开（ADR 0512、0528）。
- **Windows 子进程 containment owner 保留：** 受管进程经 `ProcessContainment` 创建、分配 Job 并恢复初始线程；命令策略、管道和取消等待仍归各 adapter。新增入口绕过该 owner 或后代残留时重开（ADR 0513）。
- **Agent 与 Memory owner 保留：** SessionActor 独占会话运行态，MemoryRuntime/Worker/Outbox 按生命周期与数据责任协作；队列顺序、取消边界和公平调度未形成可证明的重复 owner。出现跨 session 状态泄漏、恢复/取消回归或同一策略漂移时重开（ADR 0214、0424、0468、0475–0476、0481）。
- **MemoryWorker / MemoryOutbox 边界保留：** durable marker/episode IO 由 Memory `MemoryOutboxStore` 提供；Agent `MemoryOutbox` 拥有 scanner、调度、retry/ack 决策与生命周期；MemoryWorker 负责组合、推理和 prefetch。只有生命周期职责再次耦合或出现重复容量/取消状态时，才按 §5.5 复核（ADR 0518）。
- **App 与 UI owner 保留：** AppState、SessionReducer、Composer、Ask/Event/Session controllers 分属启动、状态、组件和流程生命周期；Router/media client 参数映射由 App builder 共享而错误降级与发布顺序留给 caller。仅在稳定后出现重复状态源、跨 session 泄漏或同类 lifecycle 回归时重开（ADR 0508–0510、0525）。
- **Tauri 输出 DTO 边界暂缓：** App-owned `SessionRecordDto`、Skills `SkillInfo` snapshot 与作为 Memory 命令 wire authority 的 `Fact` 各有明确 owner；MCP settings snapshot 暴露编辑所需字段并遮蔽环境变量，内部 refresh plan 不进入 IPC（ADR 0357、0720；详细字段见跨层输出契约清单）。当前没有输出字段漂移或额外消费者证据；只有 renderer 需要独立 shape、出现字段外泄或出现新消费者时再重开。
- **Common 与 ToolRunService 边界暂缓：** Common 继续作为无业务编排的共享基础 crate，`ConfigService` 仅保留 ADR 0068 定义的配置快照/串行 patch/原子持久化例外；ToolRunService 继续协调 Tools、Memory store、Agent 授权和 App lifecycle 的纵向流程。依赖方向、claim/cancel/cleanup/end 语义分别有 owner 与 ADR（0068、0359、0424、0507、0514）；没有收口后同类不变量复发。若出现重复权威状态、反向依赖、同一不变量回归或可证实的独立消费者收益，再按 §5.5 重开。

**体量准入：** 路线图不保留过期行数或提交频率快照。若 §5.5 出现符合条件的候选，再按当前源码与同环境测量职责、依赖和维护/构建收益；体量只触发复核，不构成拆分依据。

### 5.4 Common 拆分与性能优化（Candidate）

只有依赖图、重复 owner 或可复现 profile 表明存在明确收益时，才另立拆分/优化任务。crate 拆分须证明独立稳定 API、单向依赖边界及实际消费者收益；不得只为减少文件行数、构建目录或 crate 大小而拆 crate。性能比较使用相同工作负载、数据规模和环境记录前后结果；没有明显改善则关闭候选，不继续微调。

### 5.5 长期候选准入与复核

长期结构治理按“有源码证据的问题队列”推进，不预设日期或全仓重写目标。进入审查的证据包括：同一业务状态被两个 owner 维护、调用链反复跨不稳定边界、重复分支已导致 bug/回归、热点在多次变更中跨职责修改、依赖图暴露反向/多余依赖、同负载 profile 显示可复现问题，或单一实现文件承载多个可独立命名的生产操作族且其测试/调用者/生命周期边界彼此不同。文件超过约 800 行仅触发复核，不单独证明要拆；候选也不必等到先发生回归才可拆。

每个候选的短 ADR/评估要记录：现有 owner 与不变量、生产代码和测试的职责分布、调用/依赖边界、预期收益及观察方式、破坏面和停止条件、适用门禁、回滚方式。实施顺序固定为：证据确认 → 接受目标与切片 → 迁一条垂直调用链 → 删除旧入口 → 跑影响面门禁 → 对比 owner/依赖/性能指标 → 独立提交并更新状态。若只移动代码、扩大公共 API、暴露事务内部、增加第二权威来源，或验证不能证明维护/运行收益，立即停止并把候选记为“不需拆分/暂缓”。

长期执行按触发信号驱动而不按日历制造工作，优先级为数据/安全/生命周期不变量故障、重复跨 owner 回归或修改耦合、依赖/API 边界问题、最后才是有同负载证据的性能优化；行数和 crate 大小不计为准入分。仓库较大时可并行委派只读审计（例如依赖图、热点职责、事务/安全不变量），可由 sub-agent 或用户授权的其他对话承担；委派范围优先只读、交付具体文件/函数与反例，主执行者必须回到源码和门禁独立核验，不把审计结论直接当成实现要求。任何时刻只实现一个 Active 切片；跨对话协作不改变路线图、契约 owner 或提交责任。审查没有合格候选时，保留“无 Active 切片”状态并等待新证据，再继续同一套复核流程。

### 5.6 长期滚动执行周期（跨迭代周期）

按问题证据推进，不按日历、crate 数或文件行数制造工作。每轮从步骤 0 复核；历史决策和实现细节以对应 ADR 为准。

| 步骤 | 工作流                       | 进入条件与交付物                                                                                                                                                | 退出条件                                                                 |
| ---- | ---------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------ |
| 0    | **证据复核与分流**           | 切片完成、同类回归出现、依赖/API 改变或准备发布时，记录问题、不变量、owner、源码证据、影响面、停止条件和适用门禁；按 §5.5 选择 Active、Deferred、关闭或无候选。 | 只保留一个 Active/Next，或明确无候选；旧快照与体量数字不得单独触发重构。 |
| 1    | **数据、安全与生命周期契约** | 仅在 owner 重复、失败/恢复结果不一致或同类故障复发时，形成失败注入与恢复场景。                                                                                  | 成功、拒绝、重启、重试和取消语义由唯一 owner 表达并经适用门禁验证。      |
| 2    | **跨层权威来源**             | 有证据显示持久数据、事件、运行态、投影或 UI 之间发生漂移，才核对并收敛具体调用链。                                                                              | 不产生双写或第二真源；保留的投影边界可从契约中解释。                     |
| 3    | **稳定模块 owner**           | 重复策略/状态已经导致回归，且存在稳定接口、独立消费者或可隔离测试时，才提取或合并模块。                                                                         | 调用与测试归属真实 owner，依赖方向和外部行为不变，收益可复核。           |
| 4    | **crate/API 边界**           | 模块 owner 已稳定且存在单向依赖边界、独立消费者或可复核构建/维护收益。                                                                                          | API 清晰、无反向依赖或重复 adapter；否则保留现 crate 边界。              |
| 5    | **性能与容量**               | 同负载 profile 复现用户相关延迟、资源或容量问题；固定场景记录优化前后指标。                                                                                     | 改善超过噪声并达到目标，否则关闭候选。                                   |

**当前执行位置：** 阶段 0–8、第二轮 Slice 1–9 与第一轮 Slice 7 均已完成；历史覆盖记录（Rust 271/271、UI/App/IPC/文档 320/320）是当时审计快照，不代表此后源码与文档没有漂移。§5.9 图谱感知召回（ADR 1359）、§5.10 Prompt 记忆召回分路（ADR 1360）与 §5.11 事实历史与按时点查询（ADR 1361）已完成；§5.12 推断事实先审核再写入（ADR 1362）、§5.13 原文证据核验与受控自动接受（ADR 1363）、§5.14 有界两跳事实图召回（ADR 1364）与 §5.15 事实现实有效时间（ADR 1365）已完成。R12 派生会话动作结果及显式刷新失败传播已完成（ADR 1431）；R04 清理唤醒与工作窗口已完成（ADR 1432），明确保留进程内 backlog 与现有重启边界，不以丢弃待办实现容量上限。R10 源码复核未形成新的结构准入证据；Tools 严格 Clippy 基线已完成（ADR 1433）；R12 结束失败的选择隔离已完成（ADR 1434）；R12 中断等待与迟到通知隔离已完成（ADR 1435）；R04 后代结束失败保留清理 owner 已完成（ADR 1436）；workspace 全目标严格 Clippy 基线已恢复（ADR 1437）；R12 执行设置选择隔离已完成（ADR 1438）；R04 inbox 注销失败的清理责任已完成（ADR 1439）；当前无 Active/Next，整体容量/重启及其余 UI 动作按证据继续复核。AppCommand confirmation/admin adapter、SessionStore 内部职责、Tools `security.rs`、Agent `tool_runner.rs`、PDF/OpenXML parser、App command DTO 与 guarded process spawn 分别见 ADR 1302–1314。AppState/runtime 与 Tauri bootstrap 经复核继续作为单一组合和生命周期 owner，不提出 crate 合并或拆分。Windows 发布验收仍按 §5.1 跟踪。

### 5.7 全项目领域术语与架构角色命名收敛（持续覆盖）

范围覆盖所有 Rust crate、Svelte/TypeScript、Tauri IPC/事件，以及架构、命名和契约文档。职责以 `docs/architecture.md` 为准，命名以 `docs/naming.md` 为准，历史决定与实现细节见对应 ADR。

本节此前的领域职责与历史完成事项保留为当时记录。第二轮源码审计确认的兼容清理与简化顺序见 §5.3 和[第二轮方案](refactor-plan-compat-design-audit-2026-10-17.md)；第一轮结构候选见[第一轮方案](refactor-plan-source-audit-2026-10-17.md)。历史覆盖表不作为当前源文件、命令 metadata 或 checker 状态的替代证据。

#### 当前审计覆盖与剩余范围

| 领域 | 当前状态 | 本轮覆盖结论与后续复核条件 |
| --- | --- | --- |
| Rust 类型、API 与架构角色 | 多轮 crate root、实现可见性、构造入口和 capability owner 审查已完成；Memory 写入用 Common 闭合 `ScheduleMode`，而持久读入保留 raw text 供恢复 quarantine（ADR 1126）；LLM 流式工具调用 DTO 显式使用 `tool_call_id` / `tool_index`（ADR 1130）；Session 集合 API 按 `list_*` 命名并用 `session_id` 表达实体参数，ToolRun 删除 helper 私有化，文件摘要读取改为具名结果（ADR 1134–1135、1138）；Memory/EventBus 实体身份参数显式命名（ADR 1139）。 | 本轮 11 个 Rust crate 的 production inventory 268/268 完成核对；原生启动加载窗按 ADR 1316 移除，当前启动失败反馈按 ADR 1317 使用静态超时页、fatal startup 提示和定向 WebView2 导航/进程监听。后续新增或改变 API 时复核可见性、消费者与 owner。 |
| Rust 调用、状态和身份 | 持久实体 ID、event/replay 写路径和若干生命周期动词已有统一规则；恢复 error partial 的无序 Thought 发布被限定到专用 helper（ADR 1081）；Memory 对负数持久 `run_id`、transcript domain append 与非 assistant `step-*` 行 fail closed（ADR 1087）；MCP server/tool 名称组件采用单射编码，防止注册表身份碰撞（ADR 1088）；App 任务以 JoinSet 收割已完成结果，同时保留活跃任务的 shutdown join（ADR 1093）；ToolRun lease subject 明确命名为 `lease_subject_id`（ADR 1096）；VAD worker 失败沿用 recording lifecycle auto-stop，stream receiver 关闭会回收 provider body reader（ADR 1102）；自动标题任务由 AgentLayer 持有，删除前 cancel/join，标题写入/事件发布与持久删除按 session lifecycle gate 串行（ADR 1105）；Agent 消费者、ToolRun completion、restore 和 router prewarm 由 task group 持有（ADR 1106）；shutdown 在 actor registry 清空前停止并 join dispatcher wrappers 和最终 cleanup retry，Messaging inbox 注销由调用方 await（ADR 1107）；prompt cache sections 改为具名字段，Memory 集合读取和实体参数按语义角色命名（ADR 1109）；取消退出最终 cursor 核验不继承已取消的 run token（ADR 1110）；MCP notification consumer 与 HTTP SSE listener 有明确 join owner（ADR 1111）；Memory 缓存 generation tombstone 有界，facts subject/all key 身份分离且失效原子化（ADR 1112）；MCP 全部 manager connection entry point 共用并发预算，配置加载的未收割任务数同时受限（ADR 1113）；InputPipeline 启动准入、终态 shutdown、native worker join 和 auto-stop identity 校验见 ADR 1114；ToolRunService runner、output emitter、scheduled worker 和 terminal/quarantine retry 由 TaskTracker 纳管，shutdown 的 admission fence、并发等待与完整 drain 见 ADR 1115；CanonicalToolCall 内部/durable 身份字段统一为 `tool_call_id`，并保留旧事件 replay alias（ADR 1128）；MCP connection reservation 由 manager 按 server name/generation 独占并统一取消清理与 shutdown drain（ADR 1129）；流式元数据及 ToolRun 局部身份字段统一标出实体/位置（ADR 1130）；Session 存储操作参数按 `session_id` 命名；Memory/EventBus API 与局部身份参数按实体命名（ADR 1135、1139）。 | 本轮调用、状态与身份项完成全量清点；后续变更复核实体 ID、位置型值与文档例外。 |
| UI、stores、controllers 与 renderer | 主要 IPC/event 实体身份已校验并在 renderer 边界映射 camelCase；ToolRun、Settings grant、Resume Ask 双重关联及交互展示字段已纳入同一规则（ADR 1074、1078）；SessionSummary 与 pending message identity 见 ADR 1082；补充卡 presentationKey、模型配置 ID 与交互生命周期见 ADR 1083；runtime session list allowlist 见 ADR 1086；MemoryRecall 使用 entity ID、UI Automation 使用 element token，MediaPlan store 有全局 session LRU 与终态清理策略（ADR 1091–1092）；Settings LLM config event 经 app mapper 进入 renderer（ADR 1108）；resume/submit 选择和 continue/rollback 的 UI 后续遵循最新用户选择代次（ADR 1125）；Skills/MCP/内置工具开关按资源串行化并在失败后读取权威状态（ADR 1133）；测试 setup 与 profile script 模块名已统一为 camelCase；PendingInteractionsMenu 的请求身份和回调参数统一为 `requestId`；长期事实列表增加分页和加载更多（ADR 1153）。 | UI production inventory 246/246 完成分类；35 个关键文件逐行复核；后续 mapper/组件变更复核身份、字段和事件顺序。 |
| Tauri IPC 与事件 | Command response 和已登记 event channel 有生成的结构级运行时校验，handler 已有 UI owner 或明确的非 UI 归属；live usage 的单次成本与 `has_cost` 一致（ADR 1078）；runtime session list 由 App DTO 明确 allowlist，不直接序列化 Agent `SessionInfo`（ADR 1086）；WebSearch id-less aggregate completion 以可选 `call_index` 映射到 UI 并保持 renderer identity（ADR 1127）；OCR 凭据命令使用 generated 闭合 `OcrCredentialFieldInput`（ADR 1131）。 | IPC scripts 3/3 与 generated command/event shape 通过复核；后续 contract 变化由 Rust source 生成并用漂移门禁核验。 |
| 配置、持久字段与恢复 | 配置值域、单位、资源上限及主要 rollback/replay 不变量已有规范和实现校验；ToolRun 持久 `kind` 与封闭枚举一致（ADR 1080）；Skill 配置仅在目录扫描成功后与 catalog 原子发布（ADR 1090）；ConfigService 变更通知每订阅者有界且不阻塞写入（ADR 1097）；Skill requirements、目录条目与配置文件读取具有硬上限和失败保留语义（ADR 1104）；Skills manifest/requirements/入口脚本采用打开句柄身份校验，requirements fingerprint 与 pip 使用同一快照，执行脚本快照上限 16 MiB（ADR 1151）；无事件流 fresh-run 不把待投递输入重复加入 prompt history，并拒绝不可由初始输入/pending marker 还原的其它消息投影（ADR 1119）；配置解析/语义校验失败后的默认值回退为只读，阻止 Settings、配置更新和凭据暂存覆盖损坏源文件（ADR 1121）；SQLite 持久 CacheAccounting 读取拒绝三值词汇之外的存储值（ADR 1124）；Credential Manager target 使用小写 `haven/`，依赖 Windows target 大小写不敏感保持既有凭据可读（ADR 1132）；配置写入与外部修改 fingerprint 复用 4 MiB 有界读写契约（ADR 1140）。 | Common/Memory/Skills 配置与恢复范围已逐域核对；本轮 partial schema v42 重置说明已同步。未来持久语义变化更新 ADR 和 reset guide。 |
| Messaging 生命周期与容量 | 新写入 envelope 字段与序列化大小共用 contract（ADR 1095）；parent child cap 按累计身份计数（ADR 1101、1116）；删除 tombstone 清理 mailbox、processing、archive 与临时正文（ADR 1116）；每 recipient 16 MiB 逻辑 pending budget，超过后拒绝新投递；不可变 backlog + 原子 cursor sidecar 以 100 行/1 MiB bounded batch 排空，整批 ack 才推进 cursor；service/tool history 上限 100，find/last-received/selective reply bounded，broadcast 显示部分失败（ADR 1117）。旧 canonical inbox 不需重置；单行超过 1 MiB fail closed 并保留源文件。operator-only `haven-inbox-recovery` 可检查当前 cursor 上的超限行，并要求 offset 与 SHA-256 同时匹配后将原始行复制到 quarantine，再推进 cursor；单行最多 16 MiB、整个 inbox quarantine 最多 64 MiB，超限和格式不兼容仍走完整 inbox reset（ADR 1328）。quarantine 证据随 session 删除；旧格式升级仍按 reset 流程处理。删除谱系 map 读取/写入上限为 64 MiB，替换使用 `.bak` 恢复先前 committed tombstone；OS `.lock` 文件持久存在并由文件句柄持锁，重置前需关闭所有共享进程（ADR 1136）。 | Messaging 4/4 production files 已核对；Envelope recipient/type 注释漂移已修正。Windows 文件替换仍归 ADR 1136/0395 相关验证和发布 gate。 |
| Tools、网络与操作系统边界 | 不可信 HTTP URL 共享目标校验、DNS pin 和跳转规则，HTTP request 限额与取消、图片 URL redirect 校验见 ADR 1098；MCP Ask/Restricted 复用 Common destination resolver，stdio read 中断后 poison/discard 并令 liveness monitor 可触发 reconnect（ADR 1103）；Registry names-only 枚举、256 条/类上限、截断标志与阻塞线程取消见 ADR 1099；raw ToolHandle getter 不进入默认生产 facade，见 ADR 1100；文件搜索分别统计遍历/读取失败并把搜索不完整状态传给 renderer（ADR 1118）；MCP manager 的连接 reservation、策略重检、代次安全清理和 shutdown drain 见 ADR 1129；Credential Manager target 使用小写 `haven/` 且无需迁移（ADR 1132）。 Shell 授权 owner 将全局与工具级文件根交集编译到 MXC，cwd 不再由 MXC 隐式扩权（ADR 1404）。 | Tools/Platform/Input production inventory 已分域核对；本轮确认并修复 VAD shutdown、MCP list refresh/SSE 等运行边界。真实 Registry 与进程树行为仍由 Windows 发布 gate 验收。 |
| 日志与诊断 | 动态敏感内容、长度单位和事件边界指标命名已收敛；Common 错误清洗器覆盖 JSON 敏感字段和 URL 密码 userinfo（ADR 1085）；Skill setup 不向模型观察传递原始安装输出或路径（ADR 1089）；Input capture 启动日志只保留采样率、声道数和格式，不记录用户自定义设备名称；静音/callback 诊断不输出样本幅值，本机路径错误不回显路径，配置 Debug 对命令、目录、URL path/query、代理例外和认证前缀使用占位符（ADR 1137）。 | 日志/错误与诊断生产路径本轮完成扫描分类，稳定失败文本与本机路径脱敏已同步；以后新增动态字段按 conventions 校验。 |
| 文档与 ADR | 命名、架构和 IPC 契约有明确权威文档；本轮修正 Rust/UI 错误日志边界、ApplicationRuntime 名称、真实 crate 依赖、Settings snake_case 例外、UI listener/TypeScript 示例及 `load_skill` 安全矩阵，并更新输出清单日期。 | 核心架构/命名/输出契约与历史 ADR 18 项完成交叉核对；按覆盖记录保存本轮结果，历史 ADR 保留历史表述。 |
| 架构合并/拆分 | 当前无 crate 级拆分/合并候选；已确认多个私有模块职责/状态 owner 候选，见 §5.3 与两轮重构方案。 | 完成排队切片后按源码与生产调用重新评估；不得用旧审计覆盖数或“高风险”标签替代当前证据。 |

#### 执行顺序与完成条件

每轮按“源码与调用点搜索 → 核对权威 owner 和边界语义 → 修正规范文档 → 实现最小一致性切片 → 运行适用门禁 → 记录 ADR 与当前状态”推进；一次只实现一个逻辑切片。跨 Rust/UI/IPC 或持久化边界的变更按项目要求运行完整门禁。审计结论须包含具体文件/符号、正反例和保留例外，不能仅以全库搜索无结果作为覆盖证明。

本节完成时，每个主要生产概念和跨层字段都有唯一规范词、唯一权威 owner 和可定位校验；剩余例外、provider 外部 ID、动态数据边界和历史兼容行为均明确登记；规范文档不再用已退役术语描述当前实现。新发现继续按 §5.5 证据准入，不以文件数量或重命名比例作为完成标准。

### 5.8 Agent capability surface 与执行边界（原范围实现完成）

CodeMode 与 MCP exposure 的目标契约见 [ADR 1163](adr/1163-tool-exposure-codemode-workflows-and-execution-boundaries.md)。周期新会话计划已按 ADR 1323 建立；ADR 1324 已将 Schedule 计划与 occurrence 移入独立 owner，Schedule 触发不再创建 ToolRun occurrence。

**统一资源授权（ADR 1428）：**明确资源允许优先于 capability 拒绝，默认只读读取继续执行，额外文件/网络/外部效果进入统一确认；Analyze 与 Unattended 均可申请。取消普通 Shell/Files 的 VCS 自动只读保护，保留显式路径拒绝、句柄安全与撤权校验。删除域名名单，HTTP/MCP HTTP 按 origin 申请，私网与 loopback 同样申请并固定 DNS 地址。Shell、Git remote、LSP、Skill、Lifecycle Hook、Everything 与 MCP stdio 共用授权 owner 和交互端口；无法逐域名约束的进程明确申请 `ProcessNetwork` 实际范围，不宣称已有透明代理。Hook 用户等待不消耗进程执行预算。

**运行时容量与清理：**Shell scratch 256 MiB、npm 512 MiB、Cargo home 2 GiB、最多八个并发 scratch；LSP/Git/MCP scratch 256 MiB，Everything 64 MiB，每目录 100,000 条目。活动 Shell 与受管进程 scratch 共享 4 GiB 字节预算，受管进程 scratch 最多 16 个；满载时最多排队 15 秒。运行中监控并终止超限进程，摘要日志仅记录容量，不含路径或内容；退出清理，重启回收残留时跳过活跃 owner 锁。共享缓存只在 Shell 空闲时重置。容量仍是轮询软限制，不是操作系统硬磁盘配额。真实 Skill Python runtime、Everything 服务 IPC、MCP server 与第三方 LSP 的 Windows 端到端验收仍按各发布 Gate 跟踪。详见 [ADR 1430](adr/1430-shared-active-scratch-budget.md)。

**Skill runtime 候选验收：**Python 解析优先选择 Program Files 下机器级 runtime，避开已在 MXC 下触发 `STATUS_ACCESS_DENIED` 的用户 LocalAppData Python Install Manager runtime；真实 Skill/MXC 端到端用例仍待有机器级 Python 的 Windows 环境验收。

**全局安全策略写入口（ADR 1427）：**新会话默认执行方式、全局文件/网络资源策略与隔离偏好通过完整 `SecurityConfig` 经 `ToolsFacade::apply_security` 应用；永久权限规则仍由同一 owner 经 typed grant 管理。Agent 通过 session event 绑定单会话执行方式。

- **工具暴露与执行模式：**MCP tool 按工具配置 `availability`、`catalog` 和 `invocation`；原生与 Code Mode adapter 可分别或同时启用。`direct` 将启用的 native adapter 放进 provider surface，`deferred` 要求先发现和显式加载，`hidden` 只从通用目录隐藏；只有 `availability=disabled` 或无对应 adapter 会阻止相应调用路径。Code Mode 使用无宿主 API 的 QuickJS 与有界子调用；每个子调用仍经 Tools 的实时授权、target、sandbox/network 和 durable journal 校验。旧 exposure 配置由 ConfigLoader 保守转换，见 ADR 1383；旧契约背景见 [ADR 1183](adr/1183-add-mcp-model-only-exposure.md) 与 [ADR 1163](adr/1163-tool-exposure-codemode-workflows-and-execution-boundaries.md)。
- **MCP resources/prompts 与周期新会话：**资源、模板、选定 URI 的更新订阅和 prompt 由用户浏览、预览和选择；Resources/Prompts 的 `input_required` 只在 feature dialog 中使用 bounded form-mode MRTR，全部能力都不进入模型可调用工具面。后台工具调用由 ToolRun 管理；Schedule 单独拥有单次触发、周期新会话计划与 occurrence。计划按 UTC 固定间隔触发，不重叠执行，错过的 tick 跳过；新会话由统一 SessionCreation owner 建立。见 [ADR 1164](adr/1164-mcp-resources-and-user-selected-prompts.md)、[ADR 1323](adr/1323-recurring-session-schedules-on-toolruns.md) 与 [ADR 1324](adr/1324-session-creation-and-background-toolrun-boundaries.md)。
**权限与进程边界：**文件、进程与网络在副作用前复核同一证明。MCP/LSP 项目目录只读，Skill setup 只写私有目录，Everything ReadOnly 使用私有 scratch；受管 MXC 进程不因默认权限不足直接拒绝，也不在未获批时回退宿主。显式禁用/拒绝、无效资源或不能安全执行的操作返回明确失败。Windows MXC 与第三方程序的实际兼容性继续列入 §5.1，不将策略测试当成端到端验收。历史隔离背景见 ADR 1368、1405、1418–1423，当前授权语义见 ADR 1428。

### 5.9 Memory recall 的图谱扩展（已完成）

当前 facts 以 `memory_nodes` 与 SPO 节点 ID 表达关系；此前召回只匹配事实文本与 embedding。ADR 1358 已完成事实类别过滤，ADR 1359 增加受限的一跳图邻居扩展：使用关键词/向量命中的 facts 作种子，忽略通用 `user` 节点，限制种子与返回行数，并复用事实类别、主体范围和敏感内容过滤。验证覆盖 subject/object 两侧邻接、种子去重、边界限制、空结果和敏感邻居；没有引入多跳遍历或模型查询分解。后续按 §5.5 重新复核下一候选。见 [ADR 1359](adr/1359-bounded-graph-aware-memory-recall.md)。

### 5.10 Prompt 记忆召回的类型分路与上下文预算（已完成）

typed memory categories 与图邻居目前已由显式 recall 查询使用，但 Agent prompt 原先将事实类别合并成一个候选池，且通过 keyword/vector 两条旁路读候选。ADR 1360 已为 Profile、Semantic、Procedural 使用共享 query embedding 的分路检索，并让各路经同一 `MemoryRecallStore.retrieve` 进入关键词、向量与一跳图扩展。Prompt 的 15 条事实预算对每类先保留最多两个高分候选，再以原有总分补足；episode 的 5 条上限、同 session 排除和总 token/字符预算不变。路由不使用 LLM 分类器，数据与 IPC 契约不变。见 [ADR 1360](adr/1360-route-prompt-memory-by-typed-kind.md)。

### 5.11 事实历史与按时点查询（已完成）

ADR 1361 定义系统记录时间（knowledge time），不将事实创建时间推断为现实有效时间。替换或语义改写前的 facts 快照由 Memory 保存；MemoryTool `history` 可按 RFC3339 时点查询，默认 search/list/recall、prompt、FTS 与 embedding 继续只读当前 `facts`。显式 forget、clear 和敏感清理在同一事务清除历史副本；维护性低置信度修剪保留快照。schema v57，旧数据库按 §4 完整备份并重置；验证细节见 [ADR 1361](adr/1361-temporal-fact-history.md)。

### 5.12 推断事实先审核再写入（已完成）

Transcript 与 compaction summary 的模型抽取现写入有界 pending proposal，并与 extraction cursor/summary marker 原子提交；提案不进入普通召回。Memory Center 提供来源、置信度、分页及接受/拒绝；接受在同一事务复用 FactGraph 规则，拒绝不改写现有 facts。fprop-* 待审上限为 500，队列满时整批回滚并保留重试位置。Schema v58，旧数据库按 §4 重置。定向 Memory、Agent 与 UI 测试，以及 strict Clippy、Svelte、格式、IPC 和 ADR index 检查通过；当时的其余全量 UI 断言结果见 [ADR 1362](adr/1362-review-inferred-fact-proposals.md)。本切片完成后暂无 Active/Next 结构目标。

### 5.13 原文证据核验与受控自动接受（已完成）

普通 transcript 候选由单独的 verifier 请求对照原始用户消息；`supported` 必须带精确引文，Agent 校验来源身份和原文子串，Memory 在事务内再核对持久化用户消息和引文。核验错误/失败、摘要来源、不确定与矛盾候选均 fail closed 到提案队列。只有原话明确支持的候选才在 extraction marker 同一事务内经 FactGraph 自动写入，并将置信度设为至少 0.9。置信度不是统计概率；不做额外公开网搜索。schema v59，旧库按 §4 重置。验证覆盖正向、伪引文、缺失/敏感证据、模型异常、事务回滚和 UI 状态；结果见 [ADR 1363](adr/1363-source-grounded-fact-validation.md)。

### 5.14 有界两跳事实图召回（已完成）

现有 ADR 1359 只扩展一跳邻居。本切片将事实图遍历上限固定为两跳：最多 16 个种子、每跳最多 16 个概念节点、最多 80 个总候选；第二跳只经过可见的一跳事实，并保持类别、主体与敏感事实过滤。无需 schema、IPC 或旧库重置变化。Memory 全量测试为 496 passed、2 ignored；workspace strict Clippy 与格式检查通过。详见 [ADR 1364](adr/1364-bounded-two-hop-fact-recall.md)。

### 5.15 事实有效时间（Valid Time，已完成）

事实区间以可选 `valid_from` / `valid_to` 表示，使用规范 UTC 毫秒时间，按 `[valid_from, valid_to)` 解释；缺失边界保持未知，不从 `created_at` 推断。`recorded_at` 独立维护 system-time 版本，`history` 与 `valid_at` 分别回答“当时记录了什么”和“事实在现实时间是否成立”。单值更正会在新值有明确开始时间时关闭已知的旧开放区间；普通召回仍只读当前 facts。显式忘记、按 ID 删除、清空与敏感清理继续擦除全部历史副本。抽取与验证只保留用户明确说出的日期；来源核验必须支持事实及时间边界，未核验时间继续留在待审提案。schema v60，旧库按 §4 重置。Memory 全量测试通过（501 passed、0 failed、2 ignored）；其余门禁结果记录在 [ADR 1365](adr/1365-fact-valid-time.md)。

## 6. 更新规则

- 阶段/候选状态在一个逻辑切片独立提交且适用验收完成后更新；受限或未执行的验证要明确写出。发布 Gate 只在当前构建与安装环境的实际验收记录齐全后关闭。
- 设计背景、替代方案、实现范围和详细测量结果写入对应 ADR；本路线图保留链接与当前结论，不复制变更日记。
- 影响数据库、配置、IPC 或用户可见安全行为时，同步更新相应契约文档与发布/重置说明。
- 每次完成一个结构切片、准备进入下一个 Candidate、或准备发版签核时，复核候选触发证据；证据已消失就关闭为“无需拆分/优化”，不让 backlog 无限累积。
- 路线图只保留一个 Next/Active 结构目标。独立发布 Gate 可并行准备；不影响发布路径的候选整理不因 Gate 开放而被阻塞。
