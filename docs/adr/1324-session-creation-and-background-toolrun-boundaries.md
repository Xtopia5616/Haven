# 1324：统一 Session 创建来源并收窄 ToolRun

日期：2026-10-18
状态：已采纳，分片实施中；本 ADR 取代 ADR 1323 中让 scheduled occurrence 使用 ToolRun 生命周期的决定，保留其调度行为契约。
范围：Session 创建与关系、Agent peer、前台 ToolCall、后台 ToolRun、Schedule、Memory schema、模型工具目录、IPC 与 UI

## 背景

当前 Session 已承载普通对话、`agent.spawn` peer 和 fork，但持久来源只区分 `user`、`agent_spawn`、`fork`。定时新会话也写为 `user`，session 创建路径分布在 ingress、fork、peer spawn 与 scheduler。SessionLineage 只表达 parent/children，不能表达更一般的 session 关联。

ToolRunService 同时拥有后台进程和定时计划/触发。`shell` 通过 `execution_mode` 在前台 ToolCall 与后台 ToolRun 间切换，而 `tool_runs` 管理工具又混合管理后台与定时行。ToolRun 因而同时表达执行方式和触发方式。

## 决定

1. **Session 是统一的持久对话实体。** 普通新会话、Agent 创建的子 Agent 会话、fork 会话和定时触发的新会话共用 Session、SessionActor、ReAct、transcript 与恢复契约。创建入口统一进入 Agent-owned SessionCreation owner；fork 的 transcript snapshot 仍由 Memory 在原子写事务中创建，但由同一创建 owner 编排。
2. **Session 创建元数据拆为三个维度。** 持久字段分别表达 `creator`（user/agent/scheduler/system）、`creation_method`（new/agent_spawn/fork/scheduled）和 typed source。来源指向已有 session、fork source message 或 schedule/run identity；字段组合由 Common/Memory typed contract 校验。定时 session 不再伪装成普通 user-created session。
3. **Session 关联独立持久化。** 增加对称的 `related` session link store，任一端都可查询和解除关联；创建元数据继续表达 AgentSpawn 与 fork 的来源/父子关系，peer 在线状态、角色、能力和 mailbox 仍由 Messaging registry 拥有。关联查询保留被删除端的 canonical identity，不以父行外键级联丢失历史。
4. **Session 工具拥有创建和关联入口。** 新的 `session` model tool 承载 Agent 创建子 session 与建立/读取关系；`agent` tool 保留 peer discovery、消息与协作控制，不再负责创建 session。用户 UI、Agent、fork 与 scheduler 均调用统一 SessionCreation owner，不直接并行维护 session 创建流程。
5. **ToolCall 与 ToolRun 按同步/后台执行方式成对。** 前台工具调用是当前 ReAct ToolCall；ToolRun 只表示可脱离当前 turn 持续、可取消且持久记录的后台工具调用。新增 `toolrun` model tool 作为明确的后台调用入口；Shell 的普通 `shell` 调用只走前台 ToolCall，不再接受 `execution_mode`。ToolRun runtime kind 收敛为 `background`。`tool_runs` 既有管理入口并入 `toolrun` 工具的 list/inspect/cancel operations。
6. **Schedule 是独立生命周期。** `schedule` 继续拥有时间/ToolRun-completion trigger、一次性计划、周期 session 计划、确认、恢复与取消；计划和 occurrence 使用 Schedule-owned ID、存储、状态事件及 UI projection，不再创建 `kind=scheduled` ToolRun。触发时调用正常前台工具路径、向既有 session 投递输入，或经 SessionCreation owner 创建新 session。周期计划仍不重叠，错过 tick 仍跳过。
7. **关联写入按来源 owner 验证。** 创建/关联输入在边界验证 canonical IDs 与 source entity ownership；不能由模型提供或伪造 creator。跨 session 关联必须由当前 session capability 授权。工具结果、确认、取消、恢复和事件的 durable identity 分别按 ToolCall、ToolRun、ScheduleRun 与 Session 契约生成。
8. **契约破坏按测试版本重置。** 更新数据库 schema、实体 ID 规范、生成的 IPC contracts、模型工具 schema、通知与 UI。旧 schema、`scheduled` ToolRun rows、旧工具名/参数和旧 ToolRun kind 不提供运行时兼容；按 `docs/release-and-reset.md` 备份后重置数据库。

## 实施顺序

1. 定义 Session 创建元数据、关系模型、来源身份与 Schedule / ToolRun 分界；更新 ADR、路线图和 reset 说明。
2. 在 Memory 中建立 typed Session creation/source/relation 存储与查询，再由 Agent 统一普通创建、peer spawn、fork、scheduled creation。
3. 增加 `session` model tool；将 `agent.spawn` 移出 `agent` tool；新增 session 关系读写并更新提示和工具目录。
4. 将单次及周期 Schedule 的计划/occurrence/恢复从 ToolRunStore/ToolRunService 移至 Schedule owner；保留已有触发、取消、确认、恢复、结果投影和 UI 行为。
5. 收窄 ToolRun 数据库、服务、completion outbox、IPC、UI 与限制配置到 background；增加 `toolrun` 后台调用/管理工具，移除 Shell 的前后台二义参数。
6. 更新生成 contracts、用户可见 session/toolrun/schedule 导航、架构文档与测试；按跨 crate/跨端/持久化门禁验收并分目的提交。

每一切片必须独立可编译、可测试和可回退；不在中间状态提交旧/新双真源。

## 后果与未采用方案

- 保留一个 Session entity 避免为子 Agent、fork 或 scheduled session 复制 Actor、transcript、恢复与 rollback 状态机。
- 保留 ToolRun 和 Schedule 的不同实体与 owner，避免“后台执行”与“何时触发”共用一个分类和值域。
- 不保留 ToolRun `scheduled` kind、Shell `execution_mode`、`agent.spawn` alias 或旧 UI TaskKind 兼容层；这会继续模糊执行方式和来源。
- Schedule occurrence 不再伪装成后台 ToolRun；其状态、结果和通知由 Schedule 契约表达。

## 验证与回滚

每个分片按其影响运行必要的 Rust/UI、IPC 生成和 schema 测试；全部切片完成后运行 workspace Rust 与 UI 完整门禁。记录任何 Windows/HCS 环境限制。回滚代码必须同时恢复对应数据库 schema 与模型工具/IPC 契约；schema v48 及以上数据库按 reset policy 重建，不交给旧二进制打开。
