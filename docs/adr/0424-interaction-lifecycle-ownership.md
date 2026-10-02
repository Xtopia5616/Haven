# ADR 0424：人工交互生命周期所有权审计

## 状态

Proposed（2026-10-02）；记录当前契约漂移与后续重构目标，尚未改变运行时所有权。

## 背景

ADR 0156 把 `InteractionRequest` 统一成 ask、工具确认和定时任务确认的共同载荷，
也定义了前端统一投影；它同时把已删除的 `ReActSnapshot.interactions` 写成持久来源。
ADR 0196 已取代该持久化决定：普通会话交互由 `SessionActor` 持有，并通过
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
2. **每个请求只有一个权威 owner。** request 必须带有明确的 typed continuation/owner；解析按
   owner 路由并返回 `resolved`、`expired`、`stale` 或可重试失败等明确结果，删除“逐个 registry
   扫描再 fallback”的隐式分发。
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
  通知报告。没有持久 session owner 的 UI/scheduled 确认不提供 session scope，后端拒绝伪造值。
- 永久与会话授权现在分别查看、逐项撤销和整组重置；允许与拒绝决定都在设置页列出。具体
  command 与数据库契约见 ADR 0425。

## 实施顺序

1. 状态转移、日志/通知契约、授权管理和 UI-only continuation 已按 ADR 0423/0425 落地，
   保持现有 durable owner 不变；移除陈旧 ADR 对 snapshot 的现状描述。
2. 把 resolve API 改为显式 owner/continuation 路由，并让确认接受与动作执行移入独立运行单元；
   一次迁移一条 owner 路径，删除旧扫描/fallback。
3. 统一后端 expiry 接口与弹窗 pending/ack 生命周期，覆盖 renderer 关闭、迟到点击、持久化失败和
   操作执行失败；session batch 登记/恢复保持事件原子性。
4. 后续若为 resolve/expiry 等终态增加通知，必须基于 owner 已接受的 lifecycle transition；
   pending 请求已使用独立 `permission_requested` 通知配置，不复用 `session_paused`。

## 验收与影响

- 每条请求可从 owner、deadline、continuation 唯一定位；终态最多提交一次。
- session 请求重启后只从 `session_events` replay；其他请求按所属领域定义恢复或失效，不从
  `InteractionRequest` DTO 猜测。
- 批次登记失败不会留下部分暂停；决定写入失败不会丢失可操作弹窗或永久挂起 session。
- 弹窗在授权决定被接受时结束，动作结果独立报告；所有 owner 都有后端期限，超时不会启动
  动作或自动重放副作用。
- 日志/通知仅包含安全的 request id、kind、owner 类别、状态和时间等元数据。
- IPC、事件、UI 与 Agent 行为变更按 `docs/development-standards.md` 补 ADR、契约检查和定向回归。

本 ADR 当前不改变数据库 schema，因此不要求重置用户数据。若后续决定持久化 scheduled 或
UI 请求，须在实施 ADR 中说明 schema 版本、旧数据重置范围与崩溃恢复语义。
