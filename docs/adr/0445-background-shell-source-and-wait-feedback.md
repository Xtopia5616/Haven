# ADR 0445：后台 Shell 来源关联与等待反馈契约

- 状态：已采纳（2026-10-03）
- 范围：ReAct 工具步骤、`shell(background=true)`、Action durable row / completion outbox、Agent 结果投影与 ActionEvent
- 关联：ADR 0393

## 背景

普通 Agent 工具调用在 ReAct 中有 `step-*` 身份，并在工具完成后同步产生普通 `ToolResult`。`shell(background=true)` 则立即返回，后续由 `ActionService` 独立启动、管理和终结 child process，再经 completion outbox 把 completed/failed 结果投影回 session。两条路径相关联，但状态生命周期不同。

Shell 启动、`actions` 检查和仅含 running background action 的列表都提示模型结束当前回复并等待自动投递；此前这些提示只共享 `next_step=end_turn`，没有说明它们引用哪些 action 或结果如何交付。后台 Action 也没有持久记录来源工具步骤，崩溃恢复与 ActionEvent 因而不能直接携带完整 provenance。ADR 0393 已确定 background/scheduled 不进入通用 executor，并为可投递终态采用既有 outbox 和 X12 transcript 投影。

## 决定

1. 保持普通工具调用与 Action 生命周期分离。同步工具仍在 ReAct 工具调用内完成并返回普通 `ToolResult`；后台 Shell 的 `step-*` 是来源身份，`act-*` 是独立 Action 身份。`ActionService` 继续拥有 child process admission、执行、取消与终态持久化；不新增通用 Job executor、统一 run 状态或跨 kind 重放。
2. `shell(background=true)` 从可信工具执行上下文取得预铸的 `step_id`，创建 Action 时将其作为可空 `actions.source_step_id` 持久化。`ActionEvent` 与终态 action-result envelope 暴露该字段；缺少来源的旧行、直接 Shell 调用及 scheduled action 保持无来源。字段仅表示来源关联，不替代 `action_id`、`session_id` 或 terminal result identity。
3. 用 Common 的 `background_wait_object` 生成统一的模型可见等待反馈：保留兼容标记 `next_step: "end_turn"`，增加 `background_wait: { kind: "action_result", action_ids: [...], delivery: "automatic" }`，并附带清晰的 `hint`。Shell 启动、running background 状态检查以及结果全为 running background 的列表复用这一形状。混合列表、scheduled action 和同步工具不发此等待标记。
4. 该等待对象是 tool-result feedback 契约。`next_step: "end_turn"` 保留给现有 ReAct response policy 识别，避免后台结果尚未投递时把模型的即时文字误当成最终完成；它不改变 Action 状态，也不创建新的 Action lifecycle transition。后台终态仍由既有 completion outbox 投递，completed/failed 结果仍使用 action id 作为稳定 `action_result_id`，通过既有 `ActionResult`/session event 投影后 ack。来源 ID 被包进已有的不可信 JSON action-result envelope；不新增平行 transcript 写路径或 UI-only transcript 事件。取消仍不创建 completion result。
5. `source_step_id` 作为持久来源合同与 terminal result metadata 保留。Action lifecycle event 仍由 App 映射为 `ActionEvent`，不把内部状态 JSON 当成 IPC DTO。

## 影响与重置

Action row 和 outbox reconcile 可以在重启后保留来源步骤；`action:created`、`action:updated`、`action:finished` 与 action history 可暴露可选的 `source_step_id`。后台等待工具结果使用统一的 `background_wait` 对象，普通同步工具输出和 scheduled result 不变。

数据库 schema 升至 v33，新增 `actions.source_step_id`。旧 schema 不做运行时迁移；升级前按 [发布与重置说明](../release-and-reset.md) 删除 `haven.db`、`haven.db-wal` 和 `haven.db-shm`。回滚需恢复旧代码/契约并按当前 reset policy 重建数据库。

## 验证

Memory 覆盖来源字段保存、读取与 action outbox crash reconciliation；Tools 覆盖 running background 状态中的来源和等待对象；Agent 覆盖终态 envelope；App 与前端 contract 覆盖 ActionEvent 字段映射。完整门禁结果由本次提交说明记录。
