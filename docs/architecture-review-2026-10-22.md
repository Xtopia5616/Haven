# Haven 分领域架构审查与修改建议

日期：2026-10-22。范围：当前工作树的静态架构审查；与[执行上下文重设计](execution-context-redesign.md)配套。本文是审查报告与设计候选，不修改当前契约、不宣布实现完成、不自动启动整仓重构。

方法：读取各领域入口、关键 owner、调用接线及相关测试，核对正常路径和失败语义。覆盖所有 workspace crate 和 UI 的主要职责边界，未逐行检查每个实现、未运行故障注入或真实 UI/Windows 验收。下文区分 **已确认行为**、**结构建议** 和 **待验证风险**；源码路径与符号是复核入口，不能将候选风险当作已复现故障。审阅时存在另一轮 MXC 修改，未纳入本轮提交或验收。

## 1. 总体结论与优先级

当前值得保留的基础：SessionActor 持有会话热状态；session_events 负责恢复；typed stores 隔离数据库；授权 proof 与执行前重检；ToolRun 和 Schedule 生命周期分离；输入采集不直接依赖模型 provider；provider 协议留在 LLM crate。无需推倒这些边界或新建一个统管所有事情的 Manager。

优先解决的是**跨 owner 发布与恢复的闭环**：配置保存后哪些消费者真正更新、UI 丢失通知后如何补齐、后台工作和资源清理如何有界结束。整体目标为：

```text
交互：语音/文本/附件 → 输入接受 → 会话与交互状态机
执行：会话 → 模型请求/工具调用 → 各领域 executor
事实：session events / task stores / memory stores / 配置各有权威
展示：有版本的查询与事件 → 前端 reducer → 页面
宿主：App 装配、启动、关闭和 IPC；Platform 实施 Windows 能力
```

| 编号 | 领域 | 判断 | 建议优先级 |
|---|---|---|---|
| R01 | 模型热切换与记忆/标题消费者 | 已完成：Agent consumers 共享热切换运行时；见 ADR 1370 | Completed |
| R02 | 配置保存与运行时应用 | 已完成：显式重试和按目标版本状态；见 ADR 1384 | Completed |
| R03 | 模型/媒体组合发布 | 已完成：共享代次句柄，ReAct 每次新请求读取最新已发布代次；见 ADR 1385/1396 | Completed |
| R04 | Session/交互/清理队列 | 主 owner 正确；清理通知使用无界通道 | P2，证明上界并隔离清理调度 |
| R05 | 事件流与前端恢复 | 聊天页以稳定 durable cursor 补齐并拒绝不稳定恢复快照；UI 自动测试通过，Windows 聚焦丢通知与 rollback 并发仍待验收 | Windows acceptance open，见 ADR 1397 |
| R06 | 语音采集与转写 | 实施录音目标冻结、有限并发、失败保留与显式重试；自动门禁和 Windows 交互仍待完成 | P2，完成故障验收 |
| R07 | 记忆抽取、检索、维护 | 存储边界合理；编排角色密集 | P2，按工作类型与数据来源收口 |
| R08 | 受管资产与清理 | 已有租约、GC gate、引用核对和孤儿清理测试；renderer 重启与失败提交仍待 Windows 交互验收 | Acceptance Open |
| R09 | ToolRun / Schedule / Workflow | 以 execution claim 划分安全重试与结果未知；自动故障矩阵待门禁 | P2，完成故障验收；Schedule UI 按 ADR 1395 豁免 |
| R10 | LLM 路由与重试 | 请求策略已有快照 | P2，限制跨层总预算并解释路由 |
| R11 | MCP / Skills / LSP | 连接、指引、执行各有生命周期 | P2，统一状态展示而保留领域 owner |
| R12 | 前端聊天与设置 | 已拆 controllers/reducer | P2，收敛动作结果与页面协调职责 |
| R13 | App / Common / Platform | 主要依赖方向合理 | P3，按职责迁移而非按行数拆 crate |
| R14 | 持久化、发布与测试治理 | schema fail-closed 与 CI 已建立 | P2，加入跨生命周期场景验收 |

优先级表示建议的处理顺序，不等同于安全漏洞评级；没有已复现的 P0 数据损坏结论。

## 2. R01：模型更新没有覆盖全部长期消费者

**证据。** [AgentLayer](../crates/agent/src/layer.rs) 构造 `RouterMemoryInferencePort::new(router.clone())` 和 `TitleGenerator::new(router)`；`replace_router` 更新 `react_engine` 并预热新 router。独立的 [RouterMemoryInferencePort](../crates/agent/src/memory_inference.rs) 持有不可替换的 `Arc<LlmRouter>`；[MemoryService](../crates/agent/src/memory_service.rs) 与 [MemoryEmbeddingIndex](../crates/agent/src/memory_index.rs) 也保存构造时 router；[TitleGenerator](../crates/agent/src/title.rs) 同样通过自己的 router 发请求。

**影响。** 对话模型已更新，不代表之后新发起的记忆 FastChat、查询 embedding、索引 embedding 和标题请求跟着更新。配置 UI 的“应用完成”可能高估覆盖范围。这是静态调用链确认的问题，尚未通过真实 provider 请求复现；不将旧在途请求继续完成误判为故障。

**建议。** 新逻辑请求从同一版本化模型运行时入口取得只读代次；一次请求内仍固定 router。初期在现有组合根和 LLM port 内完成，不新增全局服务定位器。embedding 切换必须保留 `vector_space_id` 隔离，旧向量不能混入新空间；重建保持有界、可取消，并报告待完成状态。

**删除/验收。** 删除需要逐个手动同步的长期 router 副本或将其明确限于请求快照。使用 fake A/B router：热切换后，新聊天、标题、记忆抽取、查询/写入 embedding 都命中 B；A 的在途请求可按原策略完成。另测凭据撤销、无 embedding 路由和向量空间切换。是否涉及索引重建/持久元数据须由实施 ADR 明确。

## 3. R02：配置落盘成功与实际生效缺少可操作的恢复

**证据。** [ConfigService](../crates/common/src/config/service.rs) 持有版本化配置；[RuntimeConfigCoordinator](../crates/app-binary/src/config_runtime.rs) 的测试 `failed_model_router_apply_keeps_durable_edit_and_same_edit_skips_retry` 明确验证：应用失败后配置保留，相同编辑按 no-op 跳过，再启动才能重新初始化。这是既有契约，不是本次发现的意外回归。

**建议。** 保留“期望配置”和“实际应用状态”的区别，提供显式 `reapply` 领域操作，对已保存版本重新构造失败的目标；无需修改无关字段触发 apply。状态至少记录目标、期望版本、实际版本、结果和净化后原因。UI 分别显示“已保存”“已生效”“等待重启/可重试”。不要用一项全局成功标记掩盖部分失败。

**验收。** 第一次依赖不可用，保存成功而 apply 失败；依赖恢复后原版本重试成功；重试与新编辑串行，旧重试不能覆盖新版本。重复成功重试幂等，部分成功不重复具有外部副作用的安装/登录动作。删除“改一遍字段才能重试”的隐式使用方式。

## 4. R03：组合运行时发布必须声明可见性边界

**证据。** `RuntimeConfigCoordinator::publish_router_runtime` 先调用 `agent.replace_router`，再 await `tools.set_router_and_media_clients`；后者允许失败。现有 `failure_after_router_publish_logs_phase_metadata_and_sanitizes_secrets` 测试已考虑发布后失败，不应声称完全无人处理。

**待验证风险。** 两步之间或第二步失败后，不同消费者可能看到不同模型/媒体代次。保存 gate 串行化写操作，并不天然阻止业务读取跨代次组合。

**建议。** 先确定允许原子切换的最小组合：router 与其匹配的媒体客户端使用同一不可变代次对象；成功 prepare 后发布一个句柄，新请求捕获一次。MCP 重连、热键安装等独立系统副作用继续分目标报告，避免承诺全设置跨系统事务。

**验收。** 在第二个发布点注入失败并并发发起聊天/媒体/记忆请求，检查新请求不会混用组合；在途请求持有旧代次。若选择允许部分发布，必须在查询/事件中报告精确实际状态，并禁止依赖不兼容组合的操作。

**实施结论（2026-10-24，ADR 1385；2026-10-25，ADR 1396）。** Router、专用媒体客户端和 `MediaConfig` 经 App 独占的 `ModelMediaPublisher` 一次切换；Agent、Memory 与 Tools 只能读取同一 `ModelMediaRuntime`。测试覆盖并发完整快照、旧代次在途 STT、router reload consumers，以及模型/媒体代次发布后注入 catalog rebuild 失败。catalog 是派生投影，失败时 registry 保留旧快照；Settings error reason 明确列出已原子发布的配置版本，媒体入口在执行时重新捕获当前代次并按当前能力 fail closed，管理诊断也从当前代次读取 router。配置代次不绑定 ReAct run、session 或后台任务：每个后续逻辑请求从共享运行时捕获最新完整快照，已开始的请求仍持有起始快照。fake A/B 集成测试覆盖同一个 ReAct run 中工具切换配置后，下一次模型请求命中 B；其余代次细节见 [ADR 1396](adr/1396-use-latest-model-generation-per-react-request.md)。

## 5. R04：会话状态保留单 owner，清理工作应有界且不阻塞主调度

**证据。** [queues](../crates/agent/src/session/queues.rs) 将 follow-up、answer、steering 交给 actor；answer 直到 durable 边界才清除 Ask，值得保留。[TerminalCleanupRetryQueue](../crates/agent/src/session/mod.rs) 用 HashMap 合并同 session 请求，但通知是 `UnboundedSender<String>`；[dispatcher](../crates/agent/src/session/dispatcher.rs) 在主 select 分支内 await `retry_terminal_cleanup`。

**判断。** 存在去重，不能据 unbounded 单词断言无限重复同一 session；但跨 session 数量上界及清理耗时对新 run admission 的影响仍须证明。

**建议。** 合并表作为待办权威，唤醒只表达“有工作”，不为每次唤醒重复携带数据；明确上界或分批从持久终态扫描。清理使用独立有界工作窗口，不在主 admission 分支做无期限清理。保留 End 与 Delete 的不同语义，不合并成通用 Job。

**验收。** 结束/删除大量会话且清理持续失败时，队列与内存保持有界，新会话仍可运行；退出不丢失必须重启补做的工作。新的 worker 必须在实施中给出并发、batch、截止期限和取消合同，不能只把 await 挪进裸 spawn。

**源码复核（2026-10-27）。** 上述证据仍成立：`session/mod.rs` 的 `TerminalCleanupRetryQueue` 以 HashMap 合并策略，仍用无界 ID 通知；`dispatcher.rs::start_dispatcher_inner` 的两个通知分支仍直接 await 清理。`status.rs::finish_terminal_cleanup` 涉及 partial、inbox、后代结束与注册表清理；shutdown 已等待 run wrappers 并最终 drain 重试，不能删除这项保证。首个候选切片应只收敛唤醒和工作窗口：合并唤醒、固定 batch、单清理 worker，保留 actor identity、策略合并和退出 drain，并用阻塞清理证明新 run 可进入 handler。该切片不能声称整体 backlog 有界或跨重启可靠：`cascade=false` 等 retry 策略仍是进程内信息，startup 只恢复 pending session；硬截断 map 会丢工作。完整 backlog/restart 契约须先确定持久标记或可恢复扫描及取消边界，再进入实现。

## 6. R05：事件传递的可靠性应按消费者声明

**证据。** [EventBus::emit](../crates/agent/src/event.rs) 顺序 await 每个 subscriber。已有 BufferedEmitter、流分片合并以及 overflow 不丢权威事件的测试，不能再建议从零新增全部机制。[MemoryPersistence::session_store](../crates/memory/src/persistence.rs) 每次产生独立 live channel；[AppState](../crates/app-binary/src/app_state.rs) 有意构造 App/命令侧与 supervisor 两个 store。

**建议。** 订阅明确分为 durable 提交通知、可丢流预览、诊断三类。对外订阅端口保证每消费者自身有序，并声明慢消费者的背压/断连/补齐策略；不要直接无界并发广播。独立 store channel 必须明确哪些写操作触发哪条通道，未来消费者不能把某个局部通道误当数据库全部提交。

**验收。** 阻塞一个 subscriber，观察其它订阅、会话提交和取消的延迟；分别覆盖 raw/buffered 接入。UI 重订阅、消息快照查询与并发新事件相遇时，以 durable cursor 补齐；流预览丢失可由最终正文校正。当前已有 sequence 去重不等同于证明 gap 自动恢复，需专门端到端测试。

## 7. R06：将录音结束与转写投递作为可观测的独立阶段

**证据。** [InputPipeline](../crates/input/src/lib.rs) 负责 capture/VAD，`RecordingLifecycleOwner` 保护 `rec-*`；[recording commands](../crates/app-binary/src/commands/recording.rs) 的 `schedule_recording_transcription` 将转写注册为 app task；[recording overlay](../ui/src/lib/recordingOverlayController.ts) 已有 lifecycle revision，不能假定没有迟到事件保护。

**建议。** 在现有 owner 下明确 Capture → Transcribe → AcceptedInput 的交接；每阶段携带 recording id、结果和取消原因，麦克风释放不等待模型。为转写设置 admission 与排队上限、独立取消入口和可解释失败状态；共享麦克风的 timed media.record 继续保留独立入口，但资源占用统一仲裁。

**验收。** A 转写未完成时开始 B；取消 A 不停止 B；A 的迟到结果不提交到用户刚选中的另一会话。新增请求必须明确绑定目标 session/新会话意图。设备拔出、长录音、VAD 自动停止、连续快速操作、退出期间转写分别测试。此处是改进与故障验证建议，未证明当前必然错投。

**实施记录（2026-10-25，ADR 1399）。** 工具栏开始录音前同步目标；Capture 冻结 session/fresh-session 意图和 selection version，后台 STT 只使用该目标。并发转写有界为 2，失败、取消、退出与 admission 满时保留 WAV，启动回收孤儿/临时文件，同一 retry file 的并发重试幂等去重。UI/Rust 自动门禁通过；真实 Windows 麦克风/VAD、会话切换与重启后重试仍开放。

## 8. R07：记忆领域按数据语义和工作职责整理

**证据。** [MemoryWorker](../crates/agent/src/memory_worker.rs) 同时关联 extraction、prefetch、dirty-memory patch、维护与 outbox；已有 semaphore 和关闭 token。[MemoryService](../crates/agent/src/memory_service.rs)、typed fact/recall/proposal stores 已分离；已有提案审核、来源核验、两跳召回及时间版本。

**建议。** 保留 Fact、Proposal、Episode、Transcript、WorkingDocument 的不同语义。按“写入事实的抽取/核验”“只读召回与 prompt 组装”“索引/保留期维护”定义内部端口和诊断，先拆清调度策略，再决定是否拆私有实现。不要将所有内容塞进一个 memory_items 表，也不为每种记忆创建独立数据库。

**验收。** 显式 forget 必须覆盖当前事实、历史、提案、索引与缓存；普通过期修剪与隐私删除区别测试。抽取失败不前移 cursor，维护不饿死前台召回；prompt 各路共享总预算，所有截断可解释。当前证据不足以宣布现有删除链有遗漏，需查明后再列 bug。

## 9. R08：资产生命周期建立一个操作入口，保留各类存储职责

**证据。** [ManagedAssetRegistry](../crates/tools/src/asset_registry.rs) 有 assets、session_leases、pending_assets 和 generated_media_gate；[managed_media](../crates/app-binary/src/commands/managed_media.rs) 拥有上传与清理；AppState 周期协调 retention；Memory 从附件引用读取预览。已有并发防护，不能简单称为重复 owner。

**建议。** 建立有名的 register/attach/release/collect 操作合同，由既有生命周期 owner 协调。App 处理 picker/上传流，Tools 解析 asset capability，Memory 持久保存引用；清理算法从 commands 适配层移到内部生命周期模块。暂不另建通用文件管理服务。

**验收。** 文件已写但未登记、登记后消息提交失败、活动租约与 GC 并发、重启后 registry 为空、引用到期及路径身份替换。删除会话与用户明确删除文件分别定义；不能只根据进程内表判断磁盘文件无主。若引入持久资产索引，需独立 ADR 证明价值、明确事务/GC 协议及 schema 重置。

**复核记录（2026-10-25，ADR 1400）。** 源码复核发现活动会话上传在 ingress 失败时可能留下 session lease，阻止当前进程的 GC 回收未引用文件。上传现在统一先持有 pending lease；接受后按实际结果绑定 session，失败时释放；durable message reference 仍是重启后保护已提交文件的依据。新增失败释放与成功绑定清理测试。现有测试另覆盖 upload staging rollback、pending/session lease 与 GC 竞争、引用读取失败 fail-closed、名称碰撞、重复孤立上传清理、注册资产被历史删除后 GC、symlink/reparse point 和源文件身份替换；本轮保留轻量 process-local registry，不引入持久资产索引。真实的消息提交失败及 renderer/应用重启交互仍在 Windows Gate 中，不能从这些单元测试推断已通过。

## 10. R09：任务、计划、编排保留三个独立生命周期

**证据。** [ToolRunService](../crates/tools/src/tool_run_service.rs) 拥有后台执行/完成确认；[ScheduleService](../crates/tools/src/schedule_service.rs) 每 500ms claim、单 batch 32、fired channel 容量 256，然后逐条 send；嵌套步骤由既有 journal 恢复。当前界限比统一 job 引擎更清楚。

**建议。** ToolRun 表达执行，Schedule 表达触发，Workflow 表达步骤图。统一的是查询中的来源关系、等待原因、取消与结果未知的语言，不是合并三张表/三种状态。claim 后到消费者接收之间的 durable 状态应可查询，明确“已触发”不等于“已开始副作用”。

**验收。** 填满 fired channel 后退出/崩溃、consumer 消失、启动恢复、错过周期和重复终态投递。检查已 claim 未投递 occurrence 的最终分类，不在没有幂等证明时自动重放。依赖 ToolRun 完成触发仅触发一次可识别 occurrence；outbox 重投沿用稳定身份。

**实施记录（2026-10-25，ADR 1398）。** channel 满时 poller 受背压；receiver 退出后回收尚未执行的 running occurrences。恢复在 SQLite immediate transaction 内与 execution claim 串行：无 claim 时使用同一 occurrence ID 安全重排队，有 claim 时记为 failed/结果未知并抑制自动重放。新增未 claim/已 claim/周期身份/消费者退出测试；Rust workspace 测试与严格 Clippy 通过，Schedule 真实 UI 不在本次 Gate（ADR 1395）。

## 11. R10：模型路由提供决策解释与完整调用预算

**证据。** [LlmRouter](../crates/llm/src/router.rs) 已有 RequestRouteDiagnostics 和 EmbeddingRouteIdentity；[request_pipeline](../crates/llm/src/request_pipeline.rs) 捕获单请求 retry/timeout 快照；Common recovery 只负责纯决策。

**建议。** 保留这些边界。诊断补充安全的最终路由/降级原因、配置代次、消耗预算，前端区分“未配置”“能力不支持”“临时不可用”。审核 logical request、endpoint fallback、stream restart、工具 retry 的期限能否组合成调用者可预期的上限；不要为了代码复用把有副作用的工具重试并进 provider 重试器。

**验收。** 429/Retry-After、连续 endpoint 失败、流已输出后重试、取消与预算耗尽；总墙钟和调用次数有界，已计费用量不因失败丢失。工具结果未知不可因 provider 层认定“可重试”而重执行。本轮没有完成所有嵌套预算路径证明，保留为待验证风险。

## 12. R11：集成的连接状态、可见性、就绪状态和授权分开

**证据。** [MCP manager](../crates/mcp/src/manager.rs) 拥有 reconcile 和连接；[SkillRegistry](../crates/skills/src/lib.rs) 提供 catalog_version、refresh 与 executable 获取；LSP server 配置属于 Tools 接线。各自资源和协议不同。

**建议。** UI 可共享一个只读健康状态展示合同，但状态须说明已配置/已连接/目录可用/依赖待安装/执行被拒绝。Skill 指令激活不自动表示脚本已安装或可执行；MCP 连接成功不授予远程工具权限。目录缓存按 server/skill identity 和版本失效，移除实例后 stale capability 必须拒绝。Code Mode/exposure 的改造沿用上一份提案。

**验收。** 同名 server 替换、断线重连、Skill 执行文件替换、半安装、凭据更新、停用时仍在执行。保持 provider 与 transport 差异留在各自适配器，不在 UI 根据错误字符串推测是否重试。

## 13. R12：前端动作编排与展示状态继续收口

**证据。** [ChatSessionController](../ui/src/lib/chatSessionController.ts) 已封装继续/中断/分支/回滚，但依赖包含大量 get/set、弹窗、滚动、刷新和 fresh intent 回调；[ChatEventController](../ui/src/lib/chatEventController.ts)、[ChatViewController](../ui/src/lib/chatViewController.ts) 与 sessionReducer 已承担部分分工。

**建议。** 让 session action 返回命名结果或 dispatch 少量完整的 typed action，页面根据结果更新 UI，减少一个操作需调用多个 setter 才保持一致的组合。滚动、DOM 和弹窗留在展示层，sessionReducer 只维护可恢复投影和 UI 选择；草稿/布局偏好有明确独立生命周期。不要退回单个万能 store/controller。

**验收。** 快速切会话后旧请求返回、继续/回滚并发、提交成功但列表刷新失败、当前会话完成自动关闭、重新打开历史与新会话意图相遇。当前已有用户选择版本和 submission token 机制，应保留；相同文本的两次独立提交不能去重。

## 14. R13：App 生命周期、Common 配置与 Platform 按职责演进

**证据。** [ApplicationRuntime](../crates/app-binary/src/runtime.rs) 有统一 tasks JoinSet、完成任务回收、关闭 gate 和两类取消方式；[ConfigService](../crates/common/src/config/service.rs) 是规范允许的有状态例外；Platform 承担 OS/FFI。现有依赖图不支持为了“Common 太大”立即拆 crate。

**建议。** App commands 保留 wire 转换和调用，领域清理/录音工作/配置应用由私有有名 owner 承担。Runtime 的 task 注册补充每工作族的 admission 和诊断；JoinSet 负责持有生命周期，并不等于并发容量限制。明确可直接 drop 的工作与必须完成取消握手的工作，保留现有两类 spawn 语义。

**验收。** 任务洪峰、重复 shutdown、启动中退出、取消握手超时和启动失败后是否允许重试。不能凭 task registry 没有全局容量就断言已存在资源耗尽：应逐入口测定 admission。Platform 不吸收业务策略，Common 不吸收后台 worker。

## 15. R14：数据库与工程治理需要场景级验收

**证据。** MemoryPersistence 隔离 Database；typed stores 已确立；schema 不同拒绝打开的测试版策略明确；[CI](../.github/workflows/ci.yml) 已覆盖 Rust/UI Linux/Windows、IPC 与 ADR/依赖检查。路线图仍保留 Windows 发布验收 Gate。

**建议。** 不把 stores 数量或 ADR 数量作为复杂度指标。增加按业务场景命名的跨边界验收：模型更新→记忆请求、录音→转写→输入、上传→消息→GC、Schedule claim→执行→终态、事件提交→UI 重建。维护当前状态索引，历史 ADR 不承担当前行为查询；发现旧文档与实现漂移时只在领域权威文档修正。

**验收。** SQLite 写满、提交后通知失败、重启、rollback、删除隐私数据、磁盘文件缺失与 schema 拒绝。真实安装包的热键、音频设备、进程隔离和退出不能由 mock 单测替代。不要为本次纯文档审查运行整套编译并宣称这些行为已验证。

## 16. 建议实施顺序与每项交付标准

1. R01（Completed，ADR 1370）：用 A/B fake router 固定 Agent、Memory 与标题 consumers 的热切换行为。
2. R02（Completed，ADR 1384）与 R03（Completed，ADR 1385）：增加显式 reapply/按目标状态，并以共享不可变代次完成模型/媒体原子发布。
3. 执行上下文提案的会话工作绑定：与后台任务和当前配置代次接线保持一致。
4. R05/R12：验证事件丢失与 UI 补齐，再收敛页面动作依赖。
5. R06/R08：转写工作与资产生命周期，按实际竞态负向测试决定拆分范围。
6. R04/R09/R10：调度、重试和取消预算，先用负载证明瓶颈再调整工作窗口。
7. R07/R11/R13/R14：随对应垂直链完成内部职责收口及真实发布验收，不开启全仓大规模重命名。

这是一组候选建议，尚未排成路线图的多个 Active/Next。每次只选一个可独立交付的切片：写出不变量与失败测试 → 迁移完整调用链 → 删除旧入口 → 跑适用 Rust/UI/IPC/安全门禁 → 更新实施 ADR/权威契约/重置说明 → 独立提交。若没有行为收益或只能靠新增第二状态真源实现，应停止该拆分。

本报告仅新增文档，不改变数据库、配置、IPC 或运行行为，无需重置；撤回只需移除报告。检查范围为 Markdown 格式、本地链接与差异内容；未运行 Rust/UI 测试、性能基准或 Windows 验收。后续实施必须分别报告实际通过的检查，不能继承本报告的静态判断为运行证明。
