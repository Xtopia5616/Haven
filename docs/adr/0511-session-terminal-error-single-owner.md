# ADR 0511：Dispatcher run 的终态错误由 SessionSupervisor 单一发布

## 状态

已采纳；实现与跨 crate / UI 门禁通过（2026-10-05）。

## 背景

ReAct 遇到致命错误时会先发布 `AgentEvent::SessionError`，随后将同一失败作为 `Err` 返回。项目 dispatcher 的 `RunHandler` 把该错误交回 `SessionSupervisor`；dispatcher 做运行态清理后又发布 `SessionEvent::SessionError`。App 将两种事件分别转换为 `session:error` 和 `session:updated`，各自生成 `occ-*`。UI 的 occurrence 去重只识别同一次发布的主、副 channel，无法识别两份独立终态事实，因此对一次失败可能重复 flush、finalize 和清理预览/会话运行态。

这不是 payload 内容重复的判断问题：ReAct 事件与 supervisor lifecycle event 来自不同 owner。不能按 session、状态或错误文本猜测它们是否同一失败。

## 决定

1. 公开 `run_session_from_id` 继续保留直接运行语义：ReAct 错误由 Agent event bus 发布。项目 dispatcher 改用显式 dispatcher-only 入口，该入口只过滤 ReAct 的 `AgentEvent::SessionError`，其余 Agent events 原样转发，且原样返回运行错误。
2. dispatcher 运行中的终态错误由 `SessionSupervisor` 作为唯一发布 owner。Supervisor 仍负责错误状态、pending action step 清理和 typed `SessionEvent::SessionError`；panic、Actor 缺失及 ReAct 之前的存储/准备失败继续走同一 supervisor 发布路径。
3. bootstrap 将 supervisor 的 `SessionEvent::SessionError` 适配为 `AgentEvent::SessionError`，放入同一个 `BufferedEmitter`，再由原 `TauriEmitter` 投影。这样终态错误排在该 run 已排队的 Agent events 之后；primary 与 secondary 仍共用一个 occurrence ID，标题缓存、Windows 通知和 sanitizer 继续使用同一实现。此适配不重新进入 Agent event bus，避免形成第三次发布。
4. 不修改公开 Tauri event 名称、payload 或 occurrence 规则，不增加持久字段或 UI 的启发式去重。

## 替代方案

- 保留两种 publisher 并由 UI 按 session/status/reason 去重：拒绝。相同会话可以有多个合法终态、错误文本不是 identity，也无法可靠判断清理副作用是否已发生。
- 每个失败传播 occurrence ID：拒绝。需要跨 AgentEvent、`anyhow::Error`、supervisor event 和 App adapter 维护第二套失败身份；单一发布 owner 已足够。
- 对所有入口统一屏蔽 ReAct 的错误事件：拒绝。直接调用 `run_session_from_id` 没有 dispatcher lifecycle publisher，屏蔽会丢失既有事件语义。
- 抑制 Agent event 但保留 bootstrap 手写 SessionError payload：拒绝。会使 TauriEmitter 的标题缓存和 Windows 通知行为与直接 Agent event 路径分叉。

## 影响与验证

- 一次 dispatcher ReAct Fatal 只由 supervisor 发布一份 typed 终态错误；Tauri primary/secondary 继续作为同一 occurrence 配对，UI terminal cleanup 只运行一次。
- 直接运行 API 仍返回原错误并发布一条 `AgentEvent::SessionError`；成功、Paused、Cancelled 与非终态 Agent events 不变。
- dispatcher panic、Actor 缺失和 ReAct 前置错误没有被过滤，仍可由 supervisor 报告。错误文案经原 `TauriEmitter` sanitizer 和通知路径。
- 无 payload/schema、数据库或恢复契约变化，无需重置用户数据。
- 回归覆盖直接运行发布、dispatcher ReAct Fatal 单 owner、supervisor event 唯一性；原有 panic handler 测试继续覆盖 dispatcher 自身失败。
- 验证通过：`cargo test --workspace --locked`（一次全量运行中 `config_runtime::tests::failure_after_router_publish_logs_phase_metadata_and_sanitizes_secrets` 未捕获日志字段；单独复跑与随后完整全量复跑均通过，期间未改该测试或配置运行时）、`cargo clippy --workspace --locked -- -D warnings`、`cargo fmt --all -- --check`、`corepack pnpm run check`、`corepack pnpm run test:run`（122 files、983 tests）、`corepack pnpm run build`、`scripts/check-ipc-events.ps1`、`scripts/check-ipc-contracts.ps1`。

## 回滚

若回滚，恢复 dispatcher handler 调用公开 `run_session_from_id`，移除 dispatcher-only error filtering，并让 bootstrap 保持 supervisor 事件的现有 DTO 发布方式。回滚会恢复 ReAct Fatal 双终态 fan-out；同步撤销本 ADR 与路线图结论，并记录重复 terminal cleanup 风险。
