# Haven 架构降复杂度重构路线图

> 状态：阶段 0–8 已完成；媒体 lifecycle、录音生命周期、Files rich-path/GC、依赖清单校验和事实敏感规则单源化已完成；当前无合格的结构代码切片；Windows 发布验收为独立开放签核门
> 更新日期：2026-10-05
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

**已完成。** `session_events` 是恢复权威；pending input 的持久路由、Ask 恢复、回滚截断和提交后 UI 发布各有明确边界。待投递输入在 durable marker 中保存 answer/follow_up 路由，恢复不重算；见 ADR 0207、0209、0210、0385、0416、0440、0458。

### 阶段 3（已完成）：存储 domain ports 与 typed projection（P1）

**已完成。** Agent 生产路径通过 typed stores/ports 访问会话、记忆和任务数据；跨层输出清单记录保留的动态 JSON 边界。见 ADR 0383、architecture-output-contract-inventory.md。

### 阶段 4（已完成）：Tools capability runtime 与 Agent ports（P1）

**已完成。** 工具执行、目录、授权和服务适配经显式 ports 接入；组合根负责构建 manager-backed adapters。见 ADR 0384、0388。

### 阶段 5（已完成）：Runtime config apply ownership（P1）

**已完成。** 配置写入 gate、apply plan、部分失败与重启恢复语义已有唯一 owner。见 ADR 0351、0372。

### 阶段 6（已完成）：LLM Router 请求对象化（P2）

**已完成。** 请求描述、能力路由、执行器与 usage owner 的契约已审计收口。见 ADR 0327–0329、0354。

### 阶段 7：统一 Job 生命周期与 MemoryRuntime（P2）

**已完成。** MemoryRuntime 由应用管理；scheduled dependency 恢复和终态 transcript 投影复用既有 outbox。通用 Job executor、统一 action deadline、owner token/续租和自动 replay 不在当前设计范围。见 ADR 0367、0392、0393。

### 阶段 8：IPC 单源生成与 UI 编排收口（P2）

**已完成其定义范围。** Rust handler/Serde DTO 生成 TypeScript command contracts；聊天 ask/input、启动恢复与滚动/observer 编排分别委托稳定 owner。事件运行时校验与授权策略仍由各领域按变更持续审查，不构成待补的全局 codegen 阶段。见 ADR 0394。

### 阶段 9：Common 边界与性能复核（条件式）

**状态：条件式候选，不是必须完成的最后一阶段。** 已有性能 profile、文件型 SQLite 容量观测及 `SQLITE_FULL` 注入测试；后续 Common 拆分或性能优化只在依赖图、重复 owner 或同负载 profile 提供明确收益证据时立项。桌面发布验收是独立签核门，见 §5.1；已有基线不能代替当前安装包验收。见 ADR 0359–0361、0404。

## 5. 未完成事项

状态按执行性质区分：**Gate** 是发布前签核条件；**Next** 是下一项结构目标；**Candidate** 尚未进入实现队列，只有证据与退出条件明确后才立项。结构性目标一次只推进一个 active slice；行数下降、文件变少或 crate 变少不是完成指标。

### 5.1 Windows 发布验收（Gate / Open）

按 ADR 0395 的验收范围，在隔离的 Windows profile 或 VM 上使用当前构建完成并记录：

- 首启、Settings、会话、工具确认、媒体、后台/定时任务、恢复与回滚的真实 UI 流程；
- 当前安装包的安装、升级、卸载，以及卸载后的用户数据保留行为；
- 物理磁盘空间耗尽时的错误分类、用户提示和恢复操作。

用最新代码和安装产物复跑适用的 Rust/UI 门禁，并在 ADR 中记录实际构建版本、schema、环境、结果与限制。ADR 0395 中早期 profile 和门禁结果是当时的历史证据，不能当作当前工作树或当前安装包的通过结论。

可以提前准备隔离 profile、安装器和物理盘耗尽环境；最终验收必须使用交互生命周期等 IPC/UI 变更完成后的最新构建。此项是发布签核门，不阻塞不影响发布路径的独立模块整理。

### 5.2 交互生命周期所有权（Complete）

ADR 0424 已于 2026-10-04 采纳，确定 session、scheduled 和 UI 直调确认的运行时 owner、终态与期限契约。当前已建立非持久 `InteractionEnvelope`、显式 owner event/IPC 投影、可选真实 session 上下文以及 Session durable append/replay 校验；resolve IPC 已改用 `request_id` 并返回 typed outcome，AppCommand 直接由 `ui_confirmations` 仲裁。ScheduledAction 已改为按 `action_id` 定位 owner-local registry，并同时校验 `request_id`；批准/副作用启动前通过 ActionService 与 `scheduled_execution_claim.<action_id>` 的持久 CAS 认领执行权。认领与取消在共享数据库上 first-wins：取消先提交则批准不能授权或执行；执行认领先提交则取消返回 false，运行中的操作正常收尾。终态事务清理认领；重启时 running action 仍失败且不 replay，并清除残余 claim。Session capability/resolve 现按 `session_id` 直接定位 actor，再在该 actor 内匹配 `request_id`，不扫描其他 actor。durable resolve append 成功后才推进 actor，失败时 pending 保持可重试；session grant 持久化先于唤醒，但 grant 与 resolve event 仍是两次独立 durable write。三个 owner 已统一采用 receipt 的绝对 `expires_at`：注册严格校验，owner timer 与迟到点击按同一期限仲裁，旧 Session replay 的无效期限立即 fail closed 并在 durable expire 写入失败时退避重试；renderer 本地期限推导、自动 deny 和 IPC `timed_out` 已删除。Session confirmation 批次在同一 SQLite 事务提交 Paused 状态与整批 durable events，避免期限竞争、写入失败或进程退出留下半批次/孤立暂停。前置兼容清理、SessionStore history façade 提取及其独立门禁已完成。此项是独立结构目标，不回写为已完成的阶段 7 工作。

运行时使用 typed owner envelope：producer 显式把 owner 附到非持久 supervisor/app event，再投影到 Tauri event/resume/resolve DTO；mapper 不从 kind 或 `session_id` 推断 owner。`InteractionRequest.session_id` 可选，但 SessionActor durable append/replay 必须拒绝缺失或不匹配的值；session-owned `Some(session_id)` 保持现有 JSON 字符串形状，持久 event 不增加 owner 字段。Session owner 使用 `session_id` + request ID；scheduled owner 用 `action_id` + request ID；AppCommand owner 用 request ID。真实 `session_id` 只表达 producer 提供的上下文，不改变 owner，也不持久化通用 owner；不增加 schema/reset。resolve IPC 将实际承载 `conf-*` request ID 的 `step_id` 改名为 `request_id`。已仲裁的 `Resolved`、`Expired`、`Stale` 使用明确的类型结果；可重试失败通过命令错误返回并保留 pending。

先迁移跨层 contract，再一次只迁一条 owner 路径：

1. **守住 durable session 契约。** 将运行时 `session_id` 改为可选上下文；append 与 replay 检查 Session owner 的内外 session ID 一致。`None` 不得进入 session event；session JSON 形状不变。
2. **显式传 owner 并更新 IPC。** Session、ScheduledAction 和 AppCommand producer 都在 runtime event 附 owner；Tauri event 与 resume projection 显式映射；同步 generated command types、事件 mapper、reducer 与 IPC 文档。未知 owner、缺失 route key 和非法 owner/context 组合 fail closed。
3. **直迁 AppCommand。** `AppState.ui_confirmations` 直接按 owner + request ID 接收 resolve/expire。先验证 receipt、target、scope 和当前策略；决定可重试失败保留 pending；被接受后先终结弹窗，再启动 app-scoped continuation。
4. **直迁 ScheduledAction（路由与执行权仲裁已完成）。** 按 `action_id` + request ID 路由；有关联 session context 时 owner 仍为 ScheduledAction。批准或已获准 operation 开始副作用前，ActionService 持有 owner execution claim；SQLite `kv_store` claim 与 action status CAS 在同一 writer 序列中仲裁取消和执行。取消先赢则不授权/不执行，执行 claim 先赢则拒绝后续取消，让 operation 收尾；终态提交清理 claim 并保留 ADR 0392 的 action/outbox 顺序和 running action 不自动 replay。cancelled action 的 pending confirmation 由 action owner 事件清除。此实现不增加 schema 或修改 durable event。
5. **直迁 Session（路由已完成）。** 按 session ID 定位 actor，不跨 actor 扫描；能力查询、普通 resolve、过期和 grant-aware resolve 均使用同一明确 owner。durable resolve append 成功后才推进 actor；append 失败仍可重试；gated tool batch 全部解决、session grant 持久化后才唤醒。grant 与 resolve event 暂非原子事务，保留当前可重试语义，不宣称二者原子提交。
6. **统一 expiry（已完成）。** pending permission confirm 登记时要求有效未来期限，owner timer 与 resolve 在同一 owner 仲裁中检查 receipt 的绝对期限；renderer 只展示该期限，IPC 不再接受 `timed_out`。Session 恢复时无效历史期限立即过期，expiry durable 写入失败时保留 pending 并重试。批量 Session confirm 以单个 SQLite 事务同时提交 Paused 状态和全部 interaction events。实现与验收细节见 ADR 0424/0423。
7. **清理 owner sentinel 与旧 fallback（已完成，2026-10-05）。** AppCommand admin/MCP/skill 授权不再使用伪 `ui` session；应用与无会话的 scheduled 通知省略 `session_id`，真实会话关联原样保留；scheduled 工具日志通过 action span 保留真实 `action_id`，不再把 `action` 写成 session。只读审计未发现活跃的 executor-first、跨 registry/actor 或字符串 stale fallback，因此不新增无效改造。实现与验证见 ADR 0424。

**退出条件：** request ID 不跨 owner 扫描/fallback；错误 owner/context 不能消费请求或触发副作用；点击与到期竞争最多接受一个终态；所有 pending confirmation 有 owner 管理的有效绝对期限；renderer 关闭、迟到点击、可重试持久化失败和 continuation 执行失败均有明确结果；Session append 失败不改变 actor；session 重启只从 `session_events` 恢复，UI/scheduled 请求不自动重放。运行时 DTO 与 UI/IPC 类型保持一致；旧 route、期限入口和测试分支删除。此方案不改 durable session payload 或 schema，无需重置；验证与切片细节见 ADR 0424/0423。

### 5.3 内部模块边界整理（持续按证据复核 / 当前无 Active 代码切片）

这不是 crate 拆分目标，按职责和稳定 owner 选择可证明有益的内部边界。首个边界已完成：`SessionStore` 的只读历史查询与 DTO 已收入私有 `session_history` 模块，公开 façade、SQL owner、查询过滤/排序/缓存和序列化保持不变。聚合 event stream 与多个投影的 `session_resume_projection` 继续留在事务协调 owner。实现约束和回滚见 [ADR 0466](adr/0466-session-history-read-facade-module.md)。第二个边界已完成：Provider schema projection 归入 adapter 私有 helper，通用 schema sanitizer 和 canonical JSON 留在 `types.rs`；实现约束与验证见 [ADR 0467](adr/0467-llm-tool-schema-projection-module.md)。第三个边界已完成：`MemoryMaintenancePass` 只借用已有 store、inference、semaphore、MemoryService；周期 schedule、worker facade 与 durable outbox lifecycle 留在原 owner；步骤、取消和失败语义不变，细节与验证见 [ADR 0468](adr/0468-memory-worker-maintenance-pass-module.md)。

ADR 0424 收口后，按证据逐个评估以下候选；同一时刻只推进一项，复核后再启动下一项：

1. **LLM provider schema projection（已完成，2026-10-05）。** 将 `llm/types.rs` 的 OpenAI-compatible object-root 与 Gemini JSON Schema 方言投影移入 adapter 私有共享 helper；通用工具参数净化、JSON canonicalization 和稳定类型仍留在 `types.rs`。provider tool-schema wire 输出、缓存身份和内部完整 schema 执行校验保持不变；实现及验证见 ADR 0467。
2. **MemoryWorker 定期 maintenance（已完成，2026-10-05；构造器依赖边界同日补正）。** maintenance 编排与 LLM predicate merge / contradiction arbitration 收口到私有 pass；构造器只接收 pass 实际使用的 store、inference、semaphore 和 MemoryService，不再取得整个 `MemoryWorker`。普通 fact/summary 提取、MemoryRuntime schedule 与 durable outbox 留在现有 owner。步骤顺序、失败聚合、取消检查点、LLM 未配置时跳过及计数语义均保持；outbox marker/ack 恢复回归仍通过。实现、补正和验证见 [ADR 0468](adr/0468-memory-worker-maintenance-pass-module.md)。
3. **App managed-media 文件生命周期（已完成，2026-10-05；ADR 0469）。** 将 `commands/recording.rs` 中上传落盘和两根媒体目录清理提取到 App 私有模块，Tauri 命令和 IPC 保持不变。唯一写锁覆盖 quota/staging/提交/lease 登记与 uploads/generated-media/staging 清理；新模块通过 SessionStore 读取 durable refs，任何读取失败都阻止两根媒体目录清理；ManagedAssetRegistry 继续拥有活动/pending lease 与 TTL，AppState 继续拥有定时调度。修复 staging 根重解析点漏检、剪贴板生成文件名未被 cleaner 识别和双根扫描短路；门禁与验收见 ADR 0469。
4. **Tools generated-media producer 与 registry lease 的并发边界（已完成，2026-10-05；ADR 0470）。** 已确认图片生成、录音、截图、剪贴板 producer 的“先落盘、后登记”可与 App GC 重叠，造成活动产物被删除。由 clone-shared `ManagedAssetRegistry` 提供读写 gate：producer 在 blocking closure 中从目标文件创建前持读 permit 到 lease/TTL 登记完成；App cleaner 把独占 permit 带入清理 closure，在 generated-media 快照前获取并持有到 unlink 完成，随后释放再扫 uploads。剪贴板批次逐文件持锁，单文件 64 MiB 有界并分块检查取消；路径 stat 不阻塞 async worker。不引入 reservation 状态，不暴露 App 锁。双向先后顺序与两边 caller cancellation 均有 channel 控制回归。
5. **录音 session ID 的 stop/cancel 交接（已完成，2026-10-05；ADR 0471）。** App voice command 与 Shell handler 共用生命周期 owner；停止或取消时在下一次 start 前分离本次 ID，并显式传入 finalizer。Timed `media.record` 不创建 App voice ID，voice 命令不接管工具采集。owner handoff 与并发 stop 单次 detach 回归测试及 Rust/UI/IPC 全门禁通过。
6. **Shell 录音 overlay controller（已完成，2026-10-05；ADR 0472）。** 将 overlay store、计时器、乐观 toolbar start/stop 和 cancel 收口到唯一 controller；`+layout` 仍拥有全局 listener、通知和 voice transcript submission，输入组件只请求 toggle。旧 `rec-*` 生命周期事件不能更改新 overlay；旧转写文本仍按原 session 提交。VAD 因 payload 没有 session ID 仍按当前 recording 状态门控。Rust/UI/IPC 全门禁通过。
7. **Files rich-path 登记与 generated-media GC 互斥（已完成，2026-10-05；ADR 0473）。** canonicalize 解析输入及 reparse/junction 别名；仅当 canonical parent 是 generated-media 根目录时，registry shared permit 才覆盖 metadata、revalidation 和 lease/TTL 登记，不锁普通外部路径，也不跨入 MediaTool/模型处理。GC-first 时 handoff 等待、文件删除后失败且不遗留 lease；handoff-first 时 cleaner 等待并看见租约后保留文件。另有外部路径不等待 gate 的回归。Rust workspace test 和严格 Clippy 通过。
8. **候选审查与依赖清单一致性门禁（已完成，2026-10-05）。** 依据 §5.5 审查 Common/Tools crate 边界、SessionStore 写侧和已列热点；没有新的依赖边、重复 owner、反复回归或性能证据支持继续拆分。审查发现架构表漏列 Agent/Tools 到 Messaging 的实际依赖，旧检查脚本又允许 App 到 Skills/MCP 的不存在边；现已让脚本直接读取架构表并与 Cargo metadata 精确比对，修正结果与停止条件见 [ADR 0474](adr/0474-architecture-dependency-inventory-gate.md)。SessionStore 写侧继续受事务/回滚不变量约束；安全矩阵因生产权限提示也依赖它，保留为运行时 owner。
9. **Memory fact sensitivity 与清理规则单源化（已完成，2026-10-05；ADR 0475）。** Rust detector 与批量 SQL purge 原先维护两份凭据规则；`LIKE` 将 GitHub/npm/DigitalOcean 前缀中的 `_` 当作单字符通配符，可能永久误删相似普通事实。现由私有 `fact_security` 持有规则，Rust 检测和 SQLite predicate 共享同一组关键词/前缀/marker，前缀用精确比较。既有 `facts` façade 和单条 SQL 删除保持；增加 detector/purge 一致性与近似前缀保留回归，无 schema/reset/API 变化。实现与验证见 [ADR 0475](adr/0475-single-source-fact-sensitivity-rules.md)。

上述完成项是历史结果，Active 只表示当前可执行的一片。最新候选审查确认：Common 拆分维持 ADR 0359 的暂缓决定；Tools crate 拆分缺少
独立依赖边界和消费者收益；SessionStore 的 append、projection 与 rollback 必须保持同事务 owner；`LOCAL_TOOL_SECURITY_MATRIX`
仍被生产权限提示路径用作 operation 名白名单，因此保留在 `security.rs`。`+page.svelte`、`SettingsView.svelte`、`admin.rs`、
`llm/router.rs`、`inbox.rs`、`crates/tools/src/lib.rs`、`crates/tools/src/builtin/mod.rs`、
`crates/agent/src/react/mod.rs`、`crates/agent/src/layer.rs` 与 `app_state.rs` 均经热点复核，未发现重复 owner、边界回归或
足以证明更大拆分收益的依赖/性能证据。`+layout.svelte` 复核发现 ReAct phase store 订阅缺少销毁清理，现已通过保留 `syncStore`
disposer 并在组件销毁时调用修复；该生命周期修正不构成布局 controller 拆分理由。当前没有 Active 结构代码切片。

观察项是 Tools 根模块少量 helper 的局部归属漂移、ReAct 媒体投影 helper 的跨模块调用，以及 AgentLayer 启动编排较密；只有它们
引发重复实现、反复回归或调用边持续扩张时才重新评估。AppState 审计确认后台初始化由 `AppState::spawn_background_init` 编排，
`bootstrap.rs` 负责触发并提供 Tauri emitter；架构文档已对齐代码。只有新 bug、职责变更 churn、依赖边或可复现 profile 信号出现
时再复核，不因文件/crate 大而排期拆分。

事件存储与 transcript projection 的内部拆分属于暂缓的高风险候选：只读历史 façade 已拆，但在写侧仍有可量化维护收益之前不启动。`SessionStore` 必须继续作为 append、物化投影和 rollback 的事务协调 owner：事件与 projection 原子提交，rollback 同时维护 `event_cursor` 和 `last_msg_at`，提交成功后才发布事件。若拆分要求上层分别写 event/projection、暴露事务细节或引入第二个恢复来源，应停止。

`haven-tools/builtin/admin.rs` 的五个管理 surface 契约有意集中，不按 operation 数量机械拆开。`llm/router.rs` 已有 request/stream executor；`security.rs` 是授权、receipt、禁用 operation 和路径沙箱 owner；`inbox.rs` 的 registry、mailbox、archive 与崩溃恢复共用文件锁，暂不拆。任一热点达到约 800 行时先区分生产职责与同文件测试，再说明保留理由或拆分边界；只搬行数、没有更清晰 owner 的工作不进入计划。每个内部整理保持外部 API、wire、schema 与运行语义不变，并独立提交。

### 5.4 Common 拆分与性能优化（Candidate）

只有依赖图、重复 owner 或可复现 profile 表明存在明确收益时，才另立拆分/优化任务。crate 拆分须证明独立稳定 API、单向依赖边界及实际消费者收益；不得只为减少文件行数、构建目录或 crate 大小而拆 crate。性能比较使用相同工作负载、数据规模和环境记录前后结果；没有明显改善则关闭候选，不继续微调。

### 5.5 长期候选准入与复核

长期结构治理按“有证据的问题队列”推进，不预设日期或全仓重写目标。候选只有在出现下列至少一项时才进入审查：同一业务状态被两个 owner 维护、调用链反复跨不稳定边界、重复分支已导致 bug/回归、某热点在多个变更中频繁发生跨职责修改、依赖图暴露反向/多余依赖，或同负载 profile 显示可复现的资源/延迟问题。文件超过约 800 行或多于两个独立职责只触发复核，不单独证明要拆。

每个候选的短 ADR/评估要记录：现有 owner 与不变量、生产代码和测试的职责分布、调用/依赖边界、预期收益及观察方式、破坏面和停止条件、适用门禁、回滚方式。实施顺序固定为：证据确认 → 接受目标与切片 → 迁一条垂直调用链 → 删除旧入口 → 跑影响面门禁 → 对比 owner/依赖/性能指标 → 独立提交并更新状态。若只移动代码、扩大公共 API、暴露事务内部、增加第二权威来源，或验证不能证明维护/运行收益，立即停止并把候选记为“不需拆分/暂缓”。

长期执行按触发信号驱动而不按日历制造工作，优先级为数据/安全/生命周期不变量故障、重复跨 owner 回归或修改耦合、依赖/API 边界问题、最后才是有同负载证据的性能优化；行数和 crate 大小不计为准入分。仓库较大时可并行委派只读审计（例如依赖图、热点职责、事务/安全不变量），但审计结论须由主执行者回到源码与门禁核验，任何时刻只实现一个 Active 切片。审查没有合格候选时，保留“无 Active 切片”状态并等待新证据，再继续同一套复核流程。

## 6. 更新规则

- 阶段/候选状态在一个逻辑切片独立提交且适用验收完成后更新；受限或未执行的验证要明确写出。发布 Gate 只在当前构建与安装环境的实际验收记录齐全后关闭。
- 设计背景、替代方案、实现范围和详细测量结果写入对应 ADR；本路线图保留链接与当前结论，不复制变更日记。
- 影响数据库、配置、IPC 或用户可见安全行为时，同步更新相应契约文档与发布/重置说明。
- 每次完成一个结构切片、准备进入下一个 Candidate、或准备发版签核时，复核候选触发证据；证据已消失就关闭为“无需拆分/优化”，不让 backlog 无限累积。
- 路线图只保留一个 Next/Active 结构目标。独立发布 Gate 可并行准备；不影响发布路径的候选整理不因 Gate 开放而被阻塞。
