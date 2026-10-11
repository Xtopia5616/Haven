# ADR 0970：命令响应身份绑定其请求 owner

## 状态

已采纳并实施。

## 背景

Session command adapters 已验证响应中的 ID 格式和部分行间关联，但仍有几类 canonical 响应可能属于另一个请求 owner：按 ID 请求的 resume projection、按会话筛选的 ToolRun history、transcript supplement result，以及 Session lineage。Resume 的 `session_steps.id` 也允许 supplement 复用对应用户消息的 `msg-*`，但 UI parser 曾只接受 `step-*`。Resume message 的 attachments、media inputs 和 managed-file representation 还包含受管 `asset-*` 引用，旧 parser 未覆盖这些路径。

## 决定

- `get_session_for_resume` 的 UI adapter 将请求 `session_id` 传给 response parser，响应 session 必须匹配。latest-session 命令无显式请求 owner，仍只校验响应自身及关联行。
- Resume step ID 接受 canonical `step-*` 和 supplement 共用的 canonical `msg-*`；ask 与 invocation step 仍只关联 `step-*`。`msg-*` step 必须与同 ID 的 resume message 对应；step-backed message 继续必须与步骤行共用 ID。
- Resume message 的 attachment asset、media input asset，以及 attachment/media-input representation 中的 `managed_file_ref` asset 均验证为 canonical `asset-*`。provider tool-call ID 和其它外部 ID保持不透明。
- `list_tool_run_history` 指定 session 时，UI mapper 仅保留 `session_id` 与请求相等且存在的行；未筛选历史和 ToolRun lifecycle event 沿用现有映射。
- `ProcessResult::Supplemented` 增加 `session_id`，command adapter 要求它与请求的 `active_session_id` 匹配。`SessionCreated` 仍允许目标 session 与请求 active ID 不同：当旧 active session 已消失时，Agent ingress 按现有契约创建新 session。
- Rollback UI 只有在回退用户消息时才向 `target_message_id` 传 `msg-*`；回退 Action/step 时该参数为 `null`。user message ID 检查复用 UI 的 canonical entity ID guard。
- `SessionLineageResponse` 显式包含查询锚点和查询 session 的 `parent_session_id`；每个 child projection 也携带其 `parent_session_id`。UI 校验锚点等于请求、返回 parent 与 parent ID 对应、每个 child 属于查询 session。父行已删除时仍允许保留 canonical `parent_session_id` 且 parent projection 为空。

## 替代方案

- 只校验每个响应 ID 是 canonical：不能发现响应属于另一个合法实体，拒绝。
- 在 UI 根据历史内容或当前 timeline 猜测缺失的 owner/关系：会引入内容推断，拒绝；wire response 应显式携带验证所需关系。
- 对 `SessionCreated` 强制等于请求的 active session：会破坏 stale active ID 触发的合法新建回退，拒绝。

## 影响与验证

- 生成 IPC contract 随 Rust DTO 自动更新；Rust、Tauri 与 UI 只通过 command adapter 和 generated DTO 表达新字段。
- 数据库 schema、配置与 durable event shape 不变，不需要数据重置。
- UI 单测覆盖 request-owner mismatch、lineage parent/child relation、supplement `msg-*` step、四类嵌套 asset 引用、ToolRun session-filter response，以及 rollback 的 message/step ID 区分。
- Rust 单测覆盖 ProcessResult serialization 和 lineage projection relationship；全量门禁按 IPC/跨 crate 变更矩阵执行。

## 回滚

恢复原 Rust response DTO 和生成 IPC contract，同时移除 UI request-owner checks。数据库与配置无需变更。
