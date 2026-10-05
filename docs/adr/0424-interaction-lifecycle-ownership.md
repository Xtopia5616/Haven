# ADR 0424：人工交互生命周期所有权审计

## 状态

已采纳，实施中（2026-10-04）。本 ADR 确定 owner 路由、终态与期限契约；实现按下述顺序逐路迁移，未完成的路径仍以当前阶段状态为准。

## 背景

ADR 0156 把 `InteractionRequest` 统一成 ask、工具确认和定时任务确认的共同载荷，
也定义了前端统一投影；它同时把已删除的 `ReActSnapshot.interactions` 写成持久来源。
[ADR 0502](0502-session-actor-event-sourced-state.md) 已取代该持久化决定：普通会话交互由 `SessionActor` 持有，并通过
`session_events` 中的 domain event 恢复。

当前一个 `InteractionRequest` 外观下仍有三条不同的确认所有权路径：

| 路径 | 当前运行时 owner | 等待期间的权威状态 |
|---|---|---|
| ReAct 工具确认 | `SessionActor` / `SessionSupervisor` | `session_events` interaction event + actor registry |
| 定时任务确认 | `SessionSupervisor.scheduled_confirms` | 进程内队列，执行结果另由 ActionService 收尾 |
| 界面直调 MCP/Skill/Admin | `AppState.ui_confirmations` | 进程内 map，保存授权请求和待执行动作 |

解析命令通过 request ID 依次查找不同 owner，失败时再尝试 UI map；超时也由 renderer、
session/scheduled 看门狗分别推进。界面直调的权限决定和获批动作仍在同一个 command 内串行，
长动作会延迟弹窗收起。前端的统一 store 因此只是统一了展示载荷，没有统一 owner、状态转移
和副作用完成时点。

## 收敛目标

1. **分清交互语义。** `ask` 是等待用户输入；permission confirm 是执行前授权。两者可以继续
   共用 shell/store 投影，但不混用状态机、超时语义或操作结果。
2. **每个请求只有一个权威 owner。** 每个 request ID 在运行时都有明确的 typed owner envelope；
   continuation 留在 owner 内部。解析按 owner 路由并返回 `resolved`、`expired`、`stale` 或可重试
   失败等明确结果，删除“逐个 registry 扫描再 fallback”的隐式分发。
3. **所有者负责原子终态。** 请求登记、批次暂停与请求写入不能部分成功；决定持久化与唤醒不能
   分离成“事件已写但 session 仍暂停”的不可恢复状态。session 的 durable source 继续是
   `session_events`；其他 owner 是否持久化由其所属领域决定，不复制第二份总交互存储。
4. **后端期限是唯一事实。** renderer 只显示 owner 给出的期限并提交用户决定；每条 permission
   路径都由后端按同一个 `expires_at` 收尾，renderer 关闭或崩溃不能让请求无限挂起。
5. **权限决定与操作执行分开。** 后端接受决定后立即结束待确认 UI，再执行对应 continuation；
   后续执行成功/失败通过其操作所属事件报告，不把长操作伪装成仍在等待授权。
6. **生命周期事件同时服务可观测性。** 每次已接受的 requested/resolved/expired/cancelled
   转移产生安全字段事件和结构化日志；通知从这些已接受的转移派生。模型输入、原始参数、
   receipt 与密钥不得进入日志或 renderer 通知。
7. **重试由后端门禁约束。** 超时结果应明确说明当前操作没有启动。是否生成后续工具调用不是
   授权边界；每个新调用仍必须重新过授权门禁，超时请求不能授权或自动重放外部副作用。

## Owner 与路由契约（提案）

路由身份属于运行时 owner envelope，不属于可恢复的 `InteractionRequest` continuation。Owner
只描述“谁持有这个待处理请求”，不携带可执行参数、授权 receipt 或权限决定：

| owner | 路由键 | 权威状态与恢复来源 |
|---|---|---|
| Session | `session_id` + request ID | 对应 `SessionActor`；从该 session 的 `session_events` 恢复 |
| ScheduledAction | `action_id` + request ID | scheduled action 的进程内 continuation；沿用 ADR 0392 的 action 重启收尾规则 |
| AppCommand | request ID | `AppState.ui_confirmations`；进程重启后失效，不重放命令 |

该类型应是仅含安全标识的 typed routing value，由产生请求的 owner 明确附在运行时 envelope，
不能从 `kind`、`session_id` 或事件到达顺序推导。它可以投影到 renderer event 并由 resolve command
带回，但只能选择后端查询哪个 owner；owner 必须再次用 request ID 检查请求仍存在、仍 pending，
并重新校验 receipt、target、scope 和当前安全策略。renderer 提供的 owner、请求种类或
session/action 关联字段均不能授予权限，也不能直接触发 continuation。

Agent supervisor 的同一进程内 `InteractionRequested` delivery event 承载 SessionActor 与
scheduled confirmation 通知，因此 producer 必须显式附上 owner，再交给 App 投影；App mapper
不按 `InteractionKind`、`session_id` 或事件到达顺序猜来源。该 delivery event 不是
`session_events` 中的 durable event。Tauri `InteractionRequestedEvent.session_id` 表达可选的关联
上下文，owner 独立携带路由身份。

持久 session event 的外层 session 关系已经确定 Session owner；不把通用 owner 或 continuation
再写进 `InteractionRequest`。AppCommand 请求没有 session owner；ScheduledAction 请求也可能没有关联
session。真实 `session_id` 只表达由可信 producer 提供的上下文，绝不能用于推断实际 owner。删除
`"ui"`/`"action"` 这类占位 session ID；若请求确有真实关联 session，则继续保留真实 ID。没有真实
session context 时不得提供 session-scope 授权；有真实 context 时，仍须由后端按 receipt、capability
target 和当前安全策略验证该 session scope。UI 与 scheduled confirmation 的执行 payload 留在各自
owner 内部，不能放入 common 类型或 IPC DTO。

`InteractionRequest.session_id` 可成为 `Option<String>`，并对 `None` 使用 `skip_serializing_if`；
`Some(session_id)` 必须仍按现有 JSON 字符串字段序列化。SessionActor 在 durable append 前强制
要求该值存在且等于 actor 所属 session，因此既有 session-owned interaction event 的 payload
形状不变。只有进程内的 AppCommand 或无真实关联 session 的 scheduled request 才能使用 `None`。

Owner 的一次 resolve/expire 操作须返回明确的 `Resolved`、`Expired`、`Stale` 或
`RetryableFailure` 结果。前三者是已仲裁的请求状态；`RetryableFailure` 表示决定尚未被接受，
pending 请求和 continuation 仍由原 owner 持有。Tauri resolve 对前三种状态返回显式枚举；可重试
失败通过命令错误返回，使 renderer 保留 pending UI。获批动作开始执行后，执行成功或失败使用其
领域结果单独报告，不再把已接受的授权决定回滚为 pending。

### 恢复语义

- Session confirmation 的 owner 由 durable event 所属的 session 确定。决定 event append 失败时，
  actor 仍保持原 pending 状态；成功 append 后才推进 actor，并在整个 gated batch 完成后唤醒。
- durable SessionActor event 必须含真实且匹配 actor 的 `session_id`；`None` 不得进入 session event log，
  这样可继续解码现有持久 payload 并防止可选上下文削弱恢复校验。
- Scheduled confirmation 只在 scheduled action 执行期持有。依据 ADR 0392，进程重启时遗留的
  `running` action 被标记失败且不自动 replay；进程内 pending confirmation 随之失效，不能从
  通用交互 DTO 重建 continuation。
- AppCommand confirmation 只在当前进程有效。重启或 renderer 关闭后不会重放动作；renderer
  关闭时仍由后端 deadline 过期并清理 owner 状态。

因此，将运行时 owner envelope 作为非持久 supervisor/app event 的路由信息，并投影到 Tauri
event/resolve IPC DTO、不写入 `session_events`，不改变数据库 schema；若未来把 owner 或 continuation 写进 durable event
payload，则属于新的持久契约，必须单独评估 schema/reset 与崩溃恢复语义，不能并入这个路由切片。

### 当前实施进度（2026-10-05）

- 已完成运行时 `InteractionOwner` / `InteractionEnvelope`；Session 与 scheduled producer 显式附带 owner，AppCommand event 显式标为 AppCommand。Owner 不序列化进 durable request。
- 已将真实 `session_id` 改为可选关联上下文；SessionActor durable append/replay 继续拒绝缺失或不匹配的 session ID，Session request 的既有 JSON 形状保持不变。
- 已完成 owner 到 Tauri event/resume DTO 的显式投影，并更新 TypeScript mapper、owner-aware reducer、Ask 过滤、scope 展示和通知分组。
- resolve IPC 已改用 `request_id`，并返回 `Resolved`、`Expired`、`Stale` 的小写 enum wire 值；renderer reducer 按结果更新或移除交互项，命令错误仍保持 pending。
- AppCommand 已直接按 `AppCommand + request_id` 查 `ui_confirmations`，不查询 Agent executor、不回退其他 owner；过期检查、receipt 校验、决定接受与 map 移除在同一 registry 锁内仲裁。回归覆盖错误 owner/request ID、重试失败保留 pending、过期状态及并发双击一次终态。
- ScheduledAction 已使用以 `action_id` 为键的 owner-local registry；能力查询、session grant、resolve 和 expiry 都同时匹配 `request_id`，错误 action ID 不扫描或消费其他 owner 的请求。
- Session 的 capability 查询、普通 resolve、expiry 与 grant-aware resolve 均按显式 `session_id` 定位单个 actor，再匹配 `request_id`，不扫描 actor registry。actor 先成功追加 durable decision event 才推进 pending 状态；append 失败仍保留请求以供重试。session grant 持久化发生在 resolve/wake 之前，但它与 durable decision event 是分开的写入，不承诺跨两者原子性。
- ScheduledAction 的执行权 claim 已落在 ActionService/ActionStore owner：批准确认使用 `action_id + request_id`，无需确认的 scheduled operation 使用 action ID。ActionService 的 `spawn_gate -> terminal_transition -> action state/DB` 锁序保护单实例；同一 SQLite writer transaction 在 `kv_store` 写 `scheduled_execution_claim.<action_id>`，取消 CAS 只有在无 claim 时才能把 waiting/running 行改成 cancelled，因此共享 DB 的多个 service 也按先提交者获胜。确认先认领后才允许 session grant、消费弹窗或启动工具；cancel 先提交则批准返回 stale、移除 owner 请求，不落授权且不执行。session grant 写失败会释放同一 request 的 claim 并保留 pending 供重试；释放本身失败时 fail closed，后续同 request 可重试。AutoApproved scheduled tool 与 continue operation 在副作用入口也认领，避免 action 已运行时取消只改终态却不能阻止副作用。
- claim 不改变 durable action/outbox 终态发布顺序：完成/失败的 action row、outbox 与 claim 删除在既有 terminal transaction 中完成；cancel terminal CAS 与发布仍归 ActionService。进程重启会把 running action 标记失败、清除残余 claim，且不 replay。新增 key 仅用现有 `kv_store`，无 schema 变更/重置。cancelled scheduled action 的 `action:finished` owner projection 同步关闭匹配的 renderer pending confirmation。
- 三个 owner 的 deadline 已统一：pending permission confirm 登记前校验可解析且尚未到期的绝对 `expires_at`，当前直接采用 receipt deadline，没有额外 renderer 上限或 ScheduledAction fallback。各 owner timer 按该值过期；resolve 也在同一个 owner 仲裁内检查时钟，迟到点击不获授权、不启动 continuation。grant-aware ScheduledAction 在 owner gate 接受决定后固定该期限判定，不会因 grant 写入耗时而把已授权决定再分类成 Expired。Session replay 中缺失/无效期限的历史 pending confirm fail closed 并立即过期；expiry event append 失败时保留 pending 并退避重试。Session confirmation 批次在一个 SQLite transaction 中提交 Paused 状态与整批 interaction events，actor 仅在 commit 成功后更新内存；回归覆盖无效期限、durable append 失败回滚及成功整批提交。AppCommand、ScheduledAction、Session 的 expiry 与点击各有定向测试。
- resolve IPC 已删除 `timed_out`；renderer 删除 `created_at + 120s` deadline 推导和自动 deny 提交，只显示 owner 的期限。事件/resume mapper 拒绝缺少或无效期限的 pending permission confirm，但 Ask 与终态 projection 可无期限。生成 command contract 与 IPC 文档已同步；schema 和 durable event 形状不变，无需重置。
- AppCommand 的 admin/MCP/skill 直接授权统一经 `app_command_authorization_request` 传 `session_id=None`；永久授权仍按同一 capability 生效，旧 `ui` key 下的 session grant 不会被读取。session scope 仍因没有持久会话而被拒绝。回归把旧 `ui` grant 预置为无效 fixture，确认 app-owned 请求仍要求确认。
- AppCommand 执行通知与没有会话关联的 scheduled 完成通知现在省略 `session_id`；Agent event、Tauri DTO 和 TypeScript mapper 一致使用可选真实关联。mapper 接受缺字段，拒绝空/畸形字段；background completion 仍必须带真实会话 ID。已有 Session 通知保持原 wire 值。
- `execute_gated` 日志不再用 `"action"` 冒充 session。scheduled fire 使用现有 `scheduled_action_fired` span；确认后续执行也建立带 `action_id` 的 span，session 字段只记录真实可选上下文。只读路由审计未发现 executor-first、跨 registry/actor 或字符串 stale fallback 的活跃调用点，因此没有为其增加空重构。
- 本切片未修改数据库 schema、配置或 durable session event；`notification:show` 的运行时 IPC 允许省略 `session_id`，同步 Rust/TypeScript mapper 和契约检查器。无需重置用户数据。
- 验证（2026-10-05）：`cargo test --workspace --locked`、受影响的 `haven-tools` 授权回归、`cargo clippy --workspace --locked -- -D warnings`、`cargo fmt --all -- --check`、UI check / 全量测试（120 files、963 tests）/ production build、`check-ipc-contracts.ps1`（79 handlers）与 `check-ipc-events.ps1`（40 channels）均通过。

### 期限唯一性与 ADR 0423 的关系（决定）

- 每个登记为 pending 的 permission confirm 都必须有所属 owner 管理的、可解析的绝对 `expires_at`。
  授权票据期限是默认来源；若 owner 另有更短的领域上限（例如 scheduled action 的等待上限），只在
  登记时取较早期限并保存最终值。缺少或无法解析期限时不得进入 pending，也不得由 renderer 用
  `created_at + 120s` 补造期限。非 pending 的合成确认项不进入 owner registry。
- deadline timer 由 owner 持有；用户决定与 deadline 到期都经同一 owner 终态仲裁。renderer 可以按
  owner 给出的 `expires_at` 显示倒计时，但不能以 `timed_out` 布尔值决定后端终态。到期后的 resolve
  返回显式 `Expired`/`Stale` 结果，不写授权、不启动 continuation，也不自动重试。
- ADR 0423 的绝对期限展示、失败后保留可重试弹窗、过期时不执行副作用等决定继续有效；本 ADR
  实施时，其 renderer 提交 `timed_out` 以及 renderer 与后端看门狗竞争仲裁的做法由本节取代。届时从
  resolve IPC 删除该字段，保留 owner 侧 expire 操作及过期的独立结果，并同步生成契约、
  前端 mapper 与 IPC 文档。resolve request 中当前名为 `step_id` 的参数实际承载 `conf-*` request ID，
  owner 路由切片应统一改名为 `request_id`；这只改变运行时 IPC，不改变 durable event 或数据库。

## 会话阻塞范围

ReAct 确认期间，只有该 session 的当前 gated tool batch 和后续 ReAct turn 进入 `Paused`；
Haven UI、其他 session 与调度器不因此停止。定时确认等待的是所属 scheduled action，界面直调
等待的是所属一次命令，两者不应伪装成会话暂停。

## 当前切片（2026-10-02）

- 删除 `Waiting for confirmation…` assistant 消息旁路。session 的 `Paused` + waiting reason
  表达等待，会话 timeline 不再插入不属于模型 transcript 的占位气泡。
- 移除 `InteractionRequest.prompt` 与 renderer event 的 `prompt`。Ask 正文只由 canonical
  transcript 保存；interaction request 保留稳定 ID、选项和 lifecycle 状态，UI 以 ID 关联。
  confirm 摘要只走 `summary`，不再复制到 prompt 或使用英文占位内容。
- scheduled confirmation 的 action title 仍保留在内部 continuation 中，用于执行结果通知；
  它不再借用通用 prompt 字段。
- 移除通用 retry nudge 里不可达的 permission/unknown-outcome 分支。授权拒绝与过期只在对应
  tool observation 中表达一次；可自动重试的 admission 仍限定为幂等、已结束且 transient 的调用。
- pending 权限请求现在经独立 `permission_requested` 配置派生应用内和 Windows 通知；桌面
  文案为通用提醒，不带 Ask 正文、工具参数或权限摘要。普通 `session_paused` 在确认等待时
  被抑制，避免同一请求发两条通知；同一会话的确认批次只发一条 Windows 提醒。
- 界面直调确认也由后端按原授权票据期限到期；renderer 关闭不会让 `ui_confirmations` 永久
  留下待执行 continuation。Always 决定先持久化，接受决定的终态事件先发出并收起弹窗，
  获批动作随后作为 app-scoped task 执行；预执行失败保持请求可重试，执行失败通过通用应用内
  通知报告。当前没有 session owner 的 UI/scheduled 确认不提供 session scope，后端拒绝伪造值。
- 永久与会话授权现在分别查看、逐项撤销和整组重置；允许与拒绝决定都在设置页列出。具体
  command 与数据库契约见 ADR 0425。

## 实施顺序

1. 状态转移、日志/通知契约、授权管理和 UI-only continuation 已按 ADR 0423/0425 落地，
   保持现有 durable owner 不变；移除陈旧 ADR 对 snapshot 的现状描述。
2. 兼容清理、SessionStore 历史 façade 提取已按 ADR 0463–0466 独立完成并通过 workspace 与 UI 门禁；
   owner 路由仍保持独立切片，避免把删除旧契约与改变运行时所有权混在一起。
3. 为 owner 与 `request_id` 建立 typed runtime envelope 和显式 resolve result；resolve IPC 将现有
   `step_id` 改为 `request_id`，并为已仲裁状态使用有类型的返回值；保留 session event 的现有 durable
   形状。一次迁移一条 owner 路径，分别以 UI 直调、scheduled action、ReAct
   confirmation 为顺序，移除每条路径对应的扫描/fallback。
4. **已实施。** 把确认接受与 continuation 执行拆开；三个 owner 使用 receipt 的有效绝对期限，
   deadline 到期与 resolve 在 owner 内仲裁。renderer 只展示 owner 提供的期限，不传 `timed_out`、
   不按创建时间推算。Session expiry append 失败时保留 pending 并重试；Session confirmation batch
   将 Paused 状态和全部请求 events 放在同一 SQLite transaction，避免部分登记与崩溃窗口。
5. 后续若为 resolve/expiry 等终态增加通知，必须基于 owner 已接受的 lifecycle transition；
   pending 请求已使用独立 `permission_requested` 通知配置，不复用 `session_paused`。

## 验收与影响

- 每条请求可从 owner、deadline、continuation 唯一定位；终态最多提交一次。
- session 请求重启后只从 `session_events` replay；其他请求按所属领域定义恢复或失效，不从
  `InteractionRequest` DTO 猜测。
- 批次登记失败不会留下部分暂停；决定写入失败不会丢失可操作弹窗或永久挂起 session。
- 弹窗在授权决定被接受时结束，动作结果独立报告；所有 owner 都有后端期限，超时不会启动
  动作或自动重放副作用。
- 日志/通知仅包含安全的 request id、kind、owner 类别、状态和时间等元数据。
- owner 路由值不含 continuation 或授权凭据；错误 owner、错误关联 ID 或重复终态只能得到有类型的 `Stale`/已终态结果，不能消费另一个 owner 的请求。ScheduledAction 的取消若先于批准取得执行权，工具副作用不得启动；确认已先被接受后的取消规则必须明确且有竞态回归。
- SessionActor 仍只追加带真实 session ID 的原有 `InteractionRequest` JSON；非持久 runtime/Tauri DTO 可省略无关联的 `session_id` 并显式携带 owner。
- renderer 倒计时、`timed_out` 输入和本地 `created_at` 推算都不能改变后端有效期限；所有 pending permission confirm 都有 owner 管理的绝对期限。
- IPC、事件、UI 与 Agent 行为变更按 `docs/development-standards.md` 补 ADR、契约检查和定向回归。

本 ADR 当前不改变数据库 schema，因此不要求重置用户数据。若后续决定持久化 scheduled 或
UI 请求，须在实施 ADR 中说明 schema 版本、旧数据重置范围与崩溃恢复语义。
