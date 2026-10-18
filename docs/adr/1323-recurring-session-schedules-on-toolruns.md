# ADR 1323：基于 ToolRun 的周期新会话计划

日期：2026-10-18
状态：已采纳并实现；本 ADR 取代 ADR 1321/1322 中 Workflow 专属持久化、定义库、进度和重试范围，并取代 ADR 1165/1166 的 Workflow 产品与执行决策。其它仍被当前代码使用的 CodeMode 和 MCP 决策不变。

## 背景

Workflow 逐步扩展成了模型可调用的 DSL、嵌套工具授权与恢复、独立 ToolRun 执行定义、定义库、周期快照、进度展示和安全重试。它与已有的后台/定时 ToolRun 重复了运行状态、持久化、取消和恢复边界，也新增了另一套执行抽象和产品入口。用户提出的周期任务需求可以由定时能力完成：到时创建一个新会话并以自然语言提示词启动 ReAct loop。

本设计将后台工作和单次计划继续收敛到 ToolRun，将重复计划作为轻量计划记录接入 schedule 工具。计划只负责何时启动；每个 occurrence 的实际执行都走标准 scheduled ToolRun 与现有 Agent 会话入口。

## 决策

1. **统一 occurrence 生命周期。** 后台执行与定时执行继续由 `tool_runs` 和 `ToolRunService` 管理。单次 `tool`、`continue` 与 `new_session` 计划均表示为一条 scheduled ToolRun。`new_session` occurrence 执行时创建一个新的用户来源 ReAct 会话，并以持久提示词作为首条输入。Agent 为 `new_session` 编写完整、自包含的执行提示词，因为新会话不继承来源会话 transcript；`body` 只作为简短的用户可见摘要。所有 scheduled prompt 的 UTF-8 大小上限为 64 KiB。
2. **重复计划只支持新会话。** `schedule` 工具接受 `repeat_interval_secs`，且只允许配合 `mode=new_session`；间隔范围为 60 秒至 1 年。周期计划使用 `sched-*` 身份，存入 Memory 的 `recurring_session_tasks`，记录来源会话、标题、用户可见摘要、提示词、UTC 间隔锚点、下次触发时间和活动 ToolRun ID；每次 occurrence 原样复用同一提示词，提示词不出现在计划列表投影中。
3. **每次触发创建标准 occurrence。** 持久调度器原子领取到期计划并创建一条 `mode=new_session` scheduled ToolRun。确认、授权、执行、失败、取消、终态通知和重启恢复继续复用 ToolRun 生命周期；发生在运行过程中的重启按现有策略将未知结果标记失败，不自动重放可能有副作用的工作。
4. **周期不重叠、不追赶。** 一条计划最多关联一个活动 occurrence。occurrence 到达终态后，基于原 UTC 锚点推进到晚于完成时间的下一次触发点；错过的 tick 跳过，不并行追赶。全局最多 64 条启用计划，单次调度轮询最多领取 32 条，并受现有 ToolRun pending admission limit 约束。
5. **删除计划使用现有取消边界。** 删除计划会阻止未来触发，并把当前 occurrence 的取消交给通用 ToolRun 取消路径。它不会增加强制中断正在执行 handler 的能力。
6. **移除 Workflow 专属机制。** 移除 Workflow DSL、合成模型工具、嵌套 Workflow 子调用授权、定义与周期快照表、节点进度、Workflow 安全重试、Workflow IPC 和 Workflow UI。CodeMode 继续使用自己的受限脚本执行和标准子调用授权边界。

## 明确不做

- 不提供通用 Job 或 DAG 执行器、命名任务定义库、节点级进度或独立重试协议。
- 不支持 `tool` 或 `continue` 模式的重复计划。
- 不支持 cron、本地墙上时间、DST、事件触发、计划暂停/恢复或原计划改频；用户可以删除后重建。
- 不新增运行时 SQLite 迁移。schema v47 的旧数据库按发布/重置规范备份后重置。

## 后果

- ToolRun 是单次后台/定时工作的唯一执行记录和恢复边界；周期计划不会另行复制 ToolRun 的状态机。
- 定时新会话从零开始构造上下文，不把创建计划的会话 transcript 当作每次运行的对话历史；执行结果可从普通 session history 和 ToolRun history 查看。
- 调度服务需要在 ToolRunStore 附加后周期性原子领取计划，并在 occurrence 完成、失败或取消时结算下一次触发时间。
- schema v47 新建 `recurring_session_tasks` 并删除 Workflow 表及 ToolRun 专属进度字段。旧数据库拒绝打开，重置会清除旧 Workflow 数据、会话和 ToolRun 历史。
- 旧 ADR 作为设计历史保留；它们描述的 Workflow 产品能力不再代表当前架构。

## 验收

- 持久化测试覆盖计划验证、原子领取、活动 occurrence 不重叠、终态结算、错过 tick 跳过、删除和重启恢复。
- ToolRun 测试覆盖 `new_session` 单次计划的执行和终态生命周期。
- UI/IPC 不再公开 Workflow 进度、重试或定义库；IPC 生成契约与 handler 集合一致。
- schema/reset 文档、ID 前缀表和当前架构路线图记录 v47 与 `sched-*` 契约。
