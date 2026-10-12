# ADR 1079：命名确认结果与会话分页边界

## 状态

已接受（2026-10-13）

## 背景

Agent 恢复确认决策的内部返回值使用位置型 `(approved, receipt)` tuple；Memory session 分页 API 的两个边界参数分别叫 `after_id` 和 `high_water`，无法从参数名直接看出它们都是 session identity。相关架构文档还将已退役的历史迁移职责写成 Memory 当前职责，并对 renderer-local Tauri `event.id` 及输出日志文件名有含混表述。

## 决定

- `confirm_decision_for` 返回 `RecordedConfirmationDecision { approved, receipt }`，调用方按字段读取批准状态和可选凭据。
- Session 分页 cursor 参数统一叫 `after_session_id` 和 `high_water_session_id`，跨 Repository、异步包装层和恢复调用保持相同名称。
- ToolRun 新 ID 在创建与后台生命周期局部变量中使用 `tool_run_id`；tracing 字段须对应实际实体身份。
- 架构文档说明 Memory 管理当前 schema、版本拒绝与 reset contract；不再称其拥有 runtime historical migrations。
- 命名文档将 Tauri renderer listener 的 `event.id` 说明为本地事件序号而非 Haven 实体 ID；日志示例和输出契约清单使用与实际字段一致的命名及单份说明。

## 替代方案

- 让调用方按 tuple 位置记忆确认字段：不采纳，因为批准状态和凭据是不同语义值。
- 用结构体上下文推断分页 ID 角色：不采纳，因为 cursor 的先后位置和 high-water 限制是两个独立边界。
- 保留错误的架构/日志文字并依赖源代码解释：不采纳，因为权威文档和日志字段是规范的一部分。

## 影响

Rust 方法签名和内部调用点改名，不改变 SQL、schema、IPC、事件或持久数据。文档修正不引入新的架构候选。

## 验证

- Agent 与 Memory 针对性 workspace tests、Clippy 与 `cargo fmt --all --check` 通过。
- 后续完整 workspace/UI 门禁按本逻辑切片执行。

## 回滚与数据重置

不涉及持久格式或 wire contract，无需重置。若回滚，须同时恢复具名结果、分页参数和受影响调用点，并保留已修正的当前架构事实。
