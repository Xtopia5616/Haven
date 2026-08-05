# Haven 对话与工具调用流程去重计划

> 创建日期: 2026-08-04
> 范围: `crates/` (Rust 后端, Tauri 2) + `ui/src/` (Svelte 5 前端)
> 目的: 消除对话/工具调用流程中的重复代码片段，统一行为，降低后续修改时分歧漂移的风险
> 注意: 文中行号为创建时的工作区快照（含未提交改动），实施时以实际文件为准

## 实施进度

- ✅ A1 `CanonicalMessage` 构造器（2026-08-04）：`system/user/assistant/tool` + `_text` 变体；react.rs 6 处、lib.rs 3 处、compactor.rs 1 处字面量全部替换；`inject_pending_context` 的 supplement/steering 双块合并为 `push_user_context`（顺带修正了 steering 块的缩进复制痕迹）
- ✅ A2 react.rs `pause_turn` helper（2026-08-04）：4 处收尾序列统一（empty-actions / final action / ask 暂停 / 循环结束），保留 ask 路径的 awaiting-answer 语义（helper 内先 set_awaiting 再 status）
- ✅ A2 lib.rs `set_task_status` helper（2026-08-04）：替换 **4 处**干净的 update+emit 配对（lib.rs:224/570/729/899，含 `reopen_task` 内层）；原进度条"6 处"估计偏高，301-307（自定义错误处理）与 832-846（中间穿插其他逻辑）两处有意保留原样
- ✅ A4 `ToolResult::summary_text()`（2026-08-04）：haven-tools 实现（tool.rs:42），react.rs（含截断）、lib.rs（reminder）、task/lib.rs 三处调用点统一
- ✅ A5 ask/notify 信号提取（2026-08-04）：`extract_ask_signal`（tool.rs:59）/ `extract_notify_signal`（tool.rs:79）/ `is_silent_action`（tool.rs:99）收归 haven-tools；react.rs 与 app-binary 共用 `is_silent_action`（app-binary 原重复计算缺少 "ask 永不静默" 规则，现已对齐，属分歧修复）
- ✅ A3 流式 LLM 调用 helper（2026-08-04）：新增 `ReActEngine::stream_llm_step`（react.rs:1293 附近），统一 spawn_chunk_consumer_raw + chunk 转发闭包 + drop tx + await consumer + 返回响应；主调用（react.rs:392）与 compaction 重试调用（react.rs:443）两处 ~25 行逐字复制合并，错误处理保留在调用方。行为微调：consumer 现在总是先排空再返回（原来主路径在 record_usage 之后才 await，重试路径在之前），chunk 事件与 usage 事件顺序统一；错误路径也会排空首个尝试的 chunk，避免与重试的新 consumer 交错
- ✅ A6 任务簿记（2026-08-04）：`TaskInfo::from_db_record`（task/lib.rs:115）、`cleanup_task_maps`（task/lib.rs:665）、`get_task`（task/lib.rs:676）三处 helper 落地；10 字段 `TaskInfo` 字面量、五处 per-task 清理三元组、三处全量扫描 find 均收归
- ✅ A7 回滚/截断（2026-08-04）：`last_user_message_ts`（messages.rs:308）、`delete_messages_after`（messages.rs:281）、`truncate_task_after`（messages.rs:325）落地；四处"最后一条用户消息时间戳"、三处成对删除 helper 化
- ✅ A8 小项（2026-08-04）：`EndpointRole::as_str/from_str/ALL` 统一 commands.rs 两处映射（172-177、1063-1070）；`connect_and_monitor`（commands.rs:64）替换 5 处 MCP 脚手架；`emit_recording_started/stopped/error`（commands.rs:107/123/141）替换 7 处发射；`confirmation_error`（commands.rs:154）合并两处；`rebuild_router`（commands.rs:191）合并 switch_model / set_reasoning_effort 尾部；`is_dangling_boundary`（lib.rs:87）暴露给 compactor.rs:138 复用；`LlmRouter::chat_with_prompt`（router.rs:505）合并 title.rs / inference.rs / compactor.rs 三处 System+User 拼装
- ✅ A9 TauriEmitter 事件映射（2026-08-05）：细化 plan 完成并实施（详见下文 A9 节），阶段 1 + 阶段 2 一次落地，前后端回归全绿
- ✅ B2 事件注册去重（2026-08-05）：细化 plan 完成（详见下文 B2 节），实施未开始
- ✅ B3 streaming.js 热路径 helper（2026-08-05）：细化 plan 完成（详见下文 B3 节），实施未开始
- ⏳ B4 小项（含 6 个子项）：未开始（详见下文 B4 节）
- ✅ B1（2026-08-05）：见下
- ✅ 审查修复（2026-08-04，共 6 项）：
  1. `rollback_task` 新增 `target_message_id` 参数（commands.rs + 前端两处 invoke 同步传入 `msgId`），孤儿回滚判定改为"目标消息是用户消息且其时间戳晚于最新分支点"——修复回滚已处理消息被孤儿劫持导致静默无效的问题；新增回归测试 `rollback_processed_user_message_with_later_orphan_wipes_target_timeline`，孤儿测试改为显式传消息 id
  2. `updateTaskMessages` 在 updater 返回同一数组引用时跳过 store 写入（每次 chunk 减少一次全量通知）；streaming.js 边界 join 改用 Map 查找（O(段数×n) → O(段数)）
  3. `rollback_task` 的 status+emit 配对改走 `set_task_status`
  4. +page.svelte 抽 `finalizeStreamBlocks` 消除 agent:action 静默/非静默分支重复
  5. `newMessage` 支持 `idPrefix`，`submitMessage` 改走工厂（消除双套 id 方案）
  6. 删除无生产调用的 `CanonicalMessage::system_text`/`assistant_text`/`tool_text`
- ✅ B1 统一消息工厂与提交路径（2026-08-05）：新建 `ui/src/lib/submit.js::submitTranscript(text, { images, voice })` 合并原 voiceSubmit.js 与 +page.svelte 各自手写的 ~20 行 process_transcript 调用；统一了"乐观气泡 → process_transcript → TaskCreated 落盘 → 失败移除气泡"主链；typed 与 voice 两路都用 `moveTaskMessages`（同一 helper 处理 `_draft` 或陈旧 task key 两种起点的迁移，typed 现在也获得 stale task 安全）；`voiceSubmit.js` 退化为 4 行 `submitVoiceTranscript(text) = submitTranscript(text, { voice: true })`；+page.svelte::`submitMessage` 缩到 12 行（仅保留页面专属收尾：同步本地 $state `activeTaskId`、`suppressAutoTask = false`、`loadTasks()`、失败 toast）。`addTaskMessage` / `newMessage` 在 +page.svelte 的导入收缩。images=null 默认不再叠加 attachments/idPrefix；空数组在 helper 内被规整为 wire 上的 `images: null`。新增 9 个 `submit.test.js` 用例覆盖两种落点、stale-task 迁移、失败回退、active id 捕获时机。
  - 分歧修复：分歧 #7（打字失败 → 历史残留 + toast vs 语音失败 → 移除气泡 + rethrow）彻底对齐：现在两条路径都"失败时移除气泡 + rethrow"。+page.svelte 内部 `catch` 块只追加自己的 toast。
  - 验证：192/192 UI 测试通过；svelte-check 0 错误；cargo clippy 未触碰后端故仅做回归（haven-tools/agent/common/app-binary clippy `-D warnings` 干净）
- ✅ B2/B3 细化（2026-08-05）：核对 +page.svelte / +layout.svelte / history/+page.svelte / tools/+page.svelte / streaming.js 实际行号，按实测更新 B2（事件注册去重）与 B3（streaming.js 热路径）所有引用
- ✅ B4 细化（2026-08-05）：核对 +layout.svelte（617 行）/ stores.js（358 行）/ reviewMessages.js（187 行）/ TaskCard.svelte（221 行）/ history/+page.svelte（809 行）/ +page.svelte（1957 行）实际行号；B4 从 4 项扩展为 6 项子节（B4.1 resetOverlay / B4.2 mergeLiveStreaming / B4.3 cutIndexForStep / B4.4 _moveMessages / B4.5 taskStatus / B4.6 syncStore）；分歧 #12 标记已解决（由 B4.2 落地）
- 验证：cargo clippy 通过；后端 106+52+38+167+148+28+49 测试通过（仅 2 个改动前即失败的网络测试）；前端 183 个测试通过，svelte-check 0 错误；rustfmt 干净
- 验证：`cargo check --workspace` / `cargo clippy -- -D warnings` 通过；haven-common 52、haven-agent 105、haven-task 28、haven-tools 336 个测试通过（`builtin::network::tests::test_network_execute_connection_refused` 为改动前即失败的环境相关测试，与本次无关）；rustfmt 仅剩改动前已存在的格式漂移（react.rs:625/832/1292/1305/1781、compactor.rs:137、tool.rs:3）
- 审查记录（2026-08-05）：/review uncommitted 共 5 项，处置如下：
  1. **文档化** `TaskConfig::default().max_steps` 30 → 200（config.rs:213）——有意提升（长多工具任务避免单次运行中途撞上限），非去重项，加注释 + 本节记录
  2. **文档化** react.rs `effective_max` resume 预算（react.rs:284）——有意行为：每次 resume 重新授予一轮完整预算，任务按"每次运行 max_steps"而非"生命周期一次 max_steps"计量；加注释 + 本节记录
  3. ✅ ToolResultCard.svelte:223/240 shell/notify 卡片标签硬编码与 `LABELS` 重复 → 统一改用 `LABELS[toolName] ?? toolName`
  4. ✅ submit.js:11 `DRAFT_KEY` 与 stores.js:55 重复定义 → 由 stores.js export、submit.js import
  5. ✅ streaming.js:12/14/18/28 四个无消费者 export（`SENTENCE_END_RE`/`isSentenceEnd`/`inUnclosedFence`/`newStreamMessage`）→ 移除 export 关键字（保留内部使用）

---

## 总体结论

对话与工具调用链路（ReAct 循环 → 消息持久化 → Tauri 事件 → 前端流式渲染）中存在大量复制粘贴式代码：后端约 18 处重复片段（多处已有实质分歧），前端约 17 处（其中 3 处已产生行为分歧，另有已知的失败气泡残留差异）。本次重构只做**结构去重与行为统一**，不改变产品功能。

实施原则：
1. 每一步完成后立即运行对应测试，防止大范围重构失控
2. 已有测试覆盖的路径优先（react.rs、reviewMessages、streaming、voiceSubmit 均有测试）
3. ~~涉及前端契约的改动（A9）单独评估，默认不做~~ — ✅ A9 已实施（2026-08-05，契约风险已实测收敛）
4. 工作区有未提交改动（react.rs、lib.rs、voiceSubmit.js、streaming.js 等），重构基于当前工作区进行，不改动逻辑

---

## 阶段 A：后端

### A1. CanonicalMessage 构造器（收益最大、零风险）
**文件:** `crates/common/src/types.rs`、`crates/agent/src/react.rs`、`crates/agent/src/lib.rs`、`crates/agent/src/compactor.rs`

- 在 `CanonicalMessage` 上新增 `user(text)` / `assistant(text, tool_calls, reasoning)` / `tool(text, tool_call_id)` 构造器
- 替换手写 8 字段字面量的 6+ 处：react.rs:205-213、235-243、252-260（inject_pending_context 三条 User）、856-863（assistant + tool_calls）、1111-1118（tool 结果）、1127-1136（"try a different approach"）；lib.rs:1093-1105（会话历史用户消息）；compactor.rs:237-246；lib.rs:2069-2078（测试 helper）
- 顺带合并 `inject_pending_context`（react.rs:182-261）中 supplement 与 steering 两个几乎逐字复制的块（前者发 Supplement 事件 + create_thought_step，后者是未缩进的复制品），以及 background-job 结果块（不发事件的问题见"分歧风险"）

### A2. 回合收尾序列抽取
**文件:** `crates/agent/src/react.rs`、`crates/agent/src/lib.rs`

react.rs 四处"收尾"重复：empty-actions（744-769）、final action（790-816）、ask 暂停（1143-1190）、循环结束（1208-1220）。每处都是：
`update_task_status(Paused/Pending) → emit AgentEvent::TaskUpdated → persist_task_message → infer() → save_branch_point → save_snapshot_with_branches`

- 抽取 `pause_turn(task_id, canonical, history, step, branch_points, emitter, final_text)` helper
- lib.rs 的 `update_task_status + emit_task_updated` 配对重复 8 处（200-203、302-306、551-563、729-745、840-845、904-912、1326-1336、1343-1345），抽 `set_task_status(task_id, status)`，状态字符串统一用 `TaskStatus::as_str()`（现 12 处手写 `"paused"`/`"pending"`）

### A3. LLM 流式调用 helper
**文件:** `crates/agent/src/react.rs`

主调用（395-426）与 compaction 重试调用（468-505）的 chunk 转发闭包逐字重复（仅 tx 变量名与警告文案不同），外加相同的外围脚手架（spawn_chunk_consumer_raw、drop tx + await consumer handle、record_usage_and_emit）。

- 抽 `stream_llm_step(role, llm_messages, tools, cancel, emitter, task_id, step_num, run_id, ...)`，返回 LlmResponse
- 空响应重试（648-655）不涉及 chunk 管道，可保持原样

### A4. ToolResult::summary_text()
**文件:** `crates/tools/src/tool.rs`、`crates/agent/src/react.rs`、`crates/agent/src/lib.rs`、`crates/task/src/lib.rs`

三处相同的 success/error 序列化：
`let text = if success { serde_json::to_string(&output)... } else { error.unwrap_or_else(|| "unknown failure".into()) };`
位于 react.rs:906-926（含 max_obs 截断）、lib.rs:344-352（含 truncate_notification）、task/lib.rs:861-870。

- 在 `ToolResult` 上实现 `summary_text()`，截断逻辑作为参数保留在调用方

### A5. ask/notify 信号提取
**文件:** `crates/agent/src/react.rs`、`crates/app-binary/src/lib.rs`

- react.rs:932-976 两段平行结构（ask 的 question/options 提取与 notify 的 title/body 提取）
- react.rs:1073-1085 显示层对 `tool_name == "ask"/"notify"` 二次匹配
- react.rs:1062-1067 计算 `silent`，TauriEmitter lib.rs:119-122 **第三次**重复该表达式

- 抽 `extract_ask_signal(&Value) -> (Option<String>, Vec<String>)` 与 `extract_notify_signal(&Value)`，`silent` 计算收归一处供双方复用

### A6. 任务簿记（task crate）
**文件:** `crates/task/src/lib.rs`、`crates/agent/src/lib.rs`、`crates/app-binary/src/commands.rs`

- 三份 10 字段 `TaskInfo` 字面量（186-197、664-675、701-712）→ `TaskInfo::from_db_record(&DbTask)`
- 五处 per-task 清理三元组 `running_tasks.remove + task_permits.remove + task_cancellations.remove`（333-335、503-508、526-528、550-552、625-627）→ `cleanup_task_maps(task_id)`
- 三处 `list_tasks().await.into_iter().find(|t| t.id == task_id)` 全量扫描（agent/lib.rs:832-838、942-948、1368-1374；commands.rs:375-390）→ `TaskExecutor::get_task(task_id)`

### A7. 回滚/截断
**文件:** `crates/agent/src/lib.rs`、`crates/memory/src/repositories/messages.rs`

- 四处"最后一条用户消息时间戳"（lib.rs:534-539、582-591、884-892、970-980）→ messages.rs 新增 `last_user_message_ts(task_id)`（可直接 SQL 实现，不必全量加载）
- 三处成对删除 `delete_messages_after/from + delete_task_steps_after`（lib.rs:677-685、689-690、898-904）→ `truncate_task_after(task_id, ts, inclusive)`

### A8. 小项
**文件:** `crates/app-binary/src/commands.rs`、`crates/tools/src/builtin/self_tool.rs`、`crates/agent/src/react.rs`、`crates/agent/src/compactor.rs`、`crates/agent/src/title.rs`、`crates/agent/src/inference.rs`、`crates/llm`

- `EndpointRole::as_str()/from_str()/ALL`（llm crate）：统一 commands.rs:59-68、self_tool.rs:159-165、react.rs:1464-1470、commands.rs:1073-1078 四套独立映射
- 暴露 `is_dangling_boundary()`（lib.rs `sanitize_canonical` 的"assistant-with-calls + 后续 Tool 消息"成对判定），供 compactor.rs `safe_end_idx`（136-149）复用，消除两套悬挂检测
- `LlmRouter::chat_with_prompt(role, system, user)`：合并 title.rs:32-49、inference.rs:141-156、compactor.rs:185-191 三处 System+User 拼装
- `confirmation_error(tool_name, params, risk_level)` helper：合并 commands.rs:566-572 与 957-963
- app-binary 内部：
  - `rebuild_router(state)`：合并 switch_model / set_reasoning_effort 的重复尾部（commands.rs:1259-1264、1304-1310）
  - MCP `connect_and_monitor` helper：替换 5 处脚手架（commands.rs:531-537、620-626、680-686+704-710、802-808），**注意 toggle_mcp_server 是先连接后保存配置，helper 必须保留该顺序语义**
  - recording 事件 3 个 helper（`emit_recording_started/stopped/error`）：替换 commands.rs 与 lib.rs 共 7 处发射（注意 commands.rs:178-186 与 211-219 是同函数内重复发射同一事件）

### A9. TauriEmitter 事件映射 ✅ 2026-08-05
**文件:** `crates/app-binary/src/lib.rs`（`TauriEmitter::emit` 已重构）、`crates/agent/src/event.rs`（未动）、`ui/src/routes/+page.svelte`、`ui/src/routes/+layout.svelte`（未动，契约兼容）

**现状核对（2026-08-05，实测）:**

`TauriEmitter::emit` 共 **15 个变体**（初稿写的 16 不准确），每个变体形态为「可选 tracing + `handle.emit(channel, json!({...}))`」，约 370 行逐字脚手架。channel 与变体一一对应：

| 变体 | channel | 备注 |
|------|---------|------|
| `Thought` | `agent:thought` | |
| `Action` | `agent:action` | payload 额外算 `silent`（`is_silent_action`，lib.rs:119） |
| `Observation` | `agent:observation` | |
| `TaskCreated(TaskInfo)` | `task:created` | 投影 `{task_id, status, title}` + Windows 通知（读 `task_created.windows`） |
| `TaskCompleted` | `task:completed` | **副发** `task:updated` + Windows 通知（读 `task_completed.windows`） |
| `TaskUpdated` | `task:updated` | 唯一主发；paused 时 warn 日志（lib.rs:253-258） |
| `TaskError` | `task:error` | **副发** `task:updated` + Windows 通知（读 `task_error.windows`） |
| `Notification` | `notification:show` | 恒发 Windows 通知 |
| `TitleUpdated` | `task:title-updated` | |
| `BalancedModelActivated` | `agent:balanced_model` | |
| `ThoughtChunk` | `agent:thought_chunk` | payload 额外带 `seq`（`chunk_seq.fetch_add`） |
| `ReasoningChunk` | `agent:reasoning_chunk` | 同上 |
| `Supplement` | `agent:supplement` | |
| `Compaction` | `agent:compaction` | |
| `Usage` | `agent:usage` | |

**三条 `task:updated` 载荷形状（分歧 #6，lib.rs:222-229 / 259-266 / 276-284）:**

| 来源 | 载荷 |
|------|------|
| `TaskCompleted` | `{task_id, status: "completed", title}` |
| `TaskUpdated` | `{task_id, status, title: ""}` |
| `TaskError` | `{task_id, status: "error", error, title: ""}` |

前端消费核对（实测）：`task:updated` 仅两处订阅——+page.svelte:803-814 读 `task_id`/`status`；+layout.svelte:338-362 读 `task_id`/`status`/`title`。**没有任何消费者读 `task:updated` 的 `error` 字段**（`error` 只在 `task:error` 通道被读，+layout.svelte:331）。

⇒ 三条形状可统一为 **`{task_id, status, title}`**（title 允许 `""`，layout 有 `data.title || data.task_id` 兜底）。从 `task:updated` 副发上删除 `error` 字段对前端**零影响**，前端 handler 无需改动即兼容统一形状。

**为何不能直接 `emit(channel, event)`（纯 derive 序列化的差异点）:**

1. 枚举的 serde 默认序列化带变体 tag（`{"Thought": {...}}`），不是 wire 形状 —— 需一层"剥 tag"取内层对象
2. `Action` 的 `silent` 是发射时计算字段（不在变体上）
3. `ThoughtChunk`/`ReasoningChunk` 的 `seq` 来自发射器状态 `self.chunk_seq`
4. `TaskCreated(TaskInfo)` 需投影（TaskInfo 的 serde 字段名是 `id` 而非 `task_id`，且含 `input` 等额外字段，不能直接泄漏）
5. 双 channel 副发与 Windows 通知是横切行为，不在事件数据里

**重构设计（两阶段，先契约后映射）:**

#### 阶段 1：`task:updated` 形状统一（后端侧，前端不动）

- `TaskCompleted` 副发保持 `{task_id, status: "completed", title}`
- `TaskUpdated` 保持 `{task_id, status, title: ""}`
- `TaskError` 副发改为 `{task_id, status: "error", title: ""}`（删 `error` 字段；`error` 仍在 `task:error` 主通道，lib.rs:268-275 不变）
- 提交并跑 `/test-ui --run` + `/check`，确认前端行为不变（重点：+layout 通知、+page banner）

#### 阶段 2：变体 → channel 映射表 + 序列化载荷

在 `TauriEmitter` 上抽纯函数，主 `emit` 缩到 ~50 行：

```rust
/// 单一事实来源：变体 → 前端订阅的 channel 名（15 行）。
fn channel(&self, event: &AgentEvent) -> &'static str {
    match event {
        AgentEvent::Thought { .. } => "agent:thought",
        AgentEvent::Action { .. } => "agent:action",
        AgentEvent::Observation { .. } => "agent:observation",
        AgentEvent::TaskCreated(_) => "task:created",
        AgentEvent::TaskCompleted { .. } => "task:completed",
        AgentEvent::TaskUpdated { .. } => "task:updated",
        AgentEvent::TaskError { .. } => "task:error",
        AgentEvent::BalancedModelActivated { .. } => "agent:balanced_model",
        AgentEvent::ThoughtChunk { .. } => "agent:thought_chunk",
        AgentEvent::ReasoningChunk { .. } => "agent:reasoning_chunk",
        AgentEvent::Supplement { .. } => "agent:supplement",
        AgentEvent::Compaction { .. } => "agent:compaction",
        AgentEvent::TitleUpdated { .. } => "task:title-updated",
        AgentEvent::Notification { .. } => "notification:show",
        AgentEvent::Usage { .. } => "agent:usage",
    }
}

/// 剥掉 serde 枚举 tag（`{"Thought": {...}}` → `{...}`），
/// 适用于除 4 个特例外的所有变体。
fn variant_payload(event: &AgentEvent) -> serde_json::Value {
    let v = serde_json::to_value(event).expect("AgentEvent 可序列化");
    v.as_object().unwrap().values().next().unwrap().clone()
}
```

payload 特例（4 个）在构造时覆盖：

- `Action`：`variant_payload` 后插入 `"silent"`（`is_silent_action`）
- `ThoughtChunk` / `ReasoningChunk`：插入 `"seq"`（`chunk_seq.fetch_add(1, Relaxed)`）
- `TaskCreated`：不走 `variant_payload`，直接 `json!({"task_id": task.id, "status": task.status.as_str(), "title": task.title})`

主 emit 保留横切行为：

```rust
async fn emit(&self, event: AgentEvent) {
    let channel = self.channel(&event);
    let payload = self.payload(&event);     // 含 4 个特例
    // 保留原有 tracing::debug!/info!/warn!（9 处，语义不变）
    let _ = self.handle.emit(channel, payload);
    self.emit_secondary(&event).await;      // TaskCompleted / TaskError 的 task:updated 副发（统一形状）
    self.maybe_show_toast(&event).await;    // 4 个通知变体
}
```

- `emit_secondary`：仅 `TaskCompleted` / `TaskError` 两个变体有副发，统一走 `{task_id, status, title}`
- `maybe_show_toast`：收敛 4 处 `notification().builder()...` 脚手架（标题兜底 `"Haven"` / `"Haven - Error"` 保留）

**收益估算：** 约 370 行 → 约 130 行（channel 15 + payload ~30 + emit_secondary/toast ~40 + 主 emit ~15 + 保留 tracing ~25）

#### 验证

- `cargo test -p haven-app-binary` + `cargo clippy -- -D warnings`（app-binary 现有测试不涉及 wire 形状，保持通过即可）
- app-binary 新增纯函数单测：`channel()` 对 15 个变体各返回预期 channel；`variant_payload` 对 `Thought` 产出 `{task_id, thought, step_number, run_id}`；`payload` 对 `Action` 含 `silent`、对 `ThoughtChunk` 含 `seq`、对 `TaskCreated` 含 `task_id` 且无 `id`/`input` 泄漏
- `/test-ui --run` + `npm run check`：通知 toast 正常、+page ask banner 正常、history 标题刷新正常
- 手动冒烟：完成 / 错误 / 暂停 / 恢复各走一遍，确认 +layout 的 waiting/ready 切换与 4 类 toast 与重构前一致

**风险与取舍：**

- `channel()` 先用 `&self` 方法；若未来有非 Tauri 前端复用再上移到 `crates/agent/src/event.rs`（本期不动）
- 不引入 `#[serde(tag = "type")]` 类重标注：会改变事件数据可读性且所有 payload 都会多一个 tag 键，收益不抵噪音
- 阶段 2 必须在阶段 1 之后做：先统一 `task:updated` 形状并验证前端，再做映射表，避免一次改动同时引入形状变化与结构重构

**实施记录（2026-08-05）：**

- 阶段 1 + 阶段 2 一次落地（前端契约风险已实测收敛：无消费者读 `task:updated` 的 `error`，删 `error` 零影响）
- 落地形态：`emit` 主函数缩到 11 行（`trace_event` → `channel` → `payload` → `handle.emit` → `emit_secondary` → `maybe_show_toast`），从 ~370 行减到 ~240 行（含全部 8 个 tracing 日志与 4 个通知变体）
- **plan 修正 1（变体数与特例数）：** 初稿写 15 个变体准确；但特例实际是 **5 个**而非 4 个——`TaskCompleted` 的 `task:completed` 主发带 `status: "completed"`（变体上无此字段）、`TaskUpdated` 的 `task:updated` 主发带 `title: ""`，均不在变体字段里，`variant_payload` 无法生成，须在 `payload` 中额外覆盖
- **plan 修正 2（测试可行性）：** `channel`/`variant_payload`/`payload` 改为关联函数（不依赖 `&self`/AppHandle），chunk 的 `seq` 由 `emit` 自增后作为 `Option<u64>` 参数传入 `payload`——纯函数化后可无需 Tauri 运行时直接单测；否则必须启用 tauri `test` feature + MockRuntime，侵入性过大
- 新增 8 个单测：`channel` 15 变体全覆盖、`variant_payload` 剥 tag、`Action` silent（含 ask 永不静默）、chunk `seq` 注入、`TaskCreated` 投影不泄漏 `id`/`input`/`summary`、`TaskCompleted`/`TaskUpdated` wire 形状保持
- 验证：app-binary 56/56 测试通过；clippy `-D warnings` 干净；app-binary rustfmt 干净；UI 192/192 测试通过；svelte-check 0 错误

---

## 阶段 B：前端

### B1. 统一消息工厂与提交路径（最高价值）✅ 2026-08-05
**文件:** `ui/src/lib/submit.js`（新建）、`ui/src/lib/voiceSubmit.js`、`ui/src/lib/stores.js`、`ui/src/routes/+page.svelte`

- ✅ 让 `newMessage()` 支持 overrides（attachments、idPrefix 等）— 由审查修复 #5 完成
- ✅ `submitMessage` 改走工厂 — 由审查修复 #5 完成
- ✅ 抽 `submitTranscript(text, { images, voice })` 统一 `process_transcript` 调用 + `TaskCreated` 落盘处理：
  - 新建 `ui/src/lib/submit.js`，承载乐观气泡、`process_transcript` 调用、`TaskCreated` 迁移、`moveTaskMessages` 起点无关迁移、失败回退 rethrow 等全部主链逻辑
  - voiceSubmit.js 退化为 `submitVoiceTranscript(text) = submitTranscript(text, { voice: true })`
  - +page.svelte::`submitMessage` 缩到 12 行：保留页面专属收尾（同步本地 `activeTaskId`、`suppressAutoTask = false`、`loadTasks()`、失败 toast），其余全部走 helper
- ✅ 消除分歧 #7：打字路径失败时乐观气泡残留 ↔ 语音失败移除气泡 — 现在两路都"失败时移除气泡 + rethrow"，+page.svelte 的 `catch` 块只追加自己的 toast
- ✅ 消除分歧 #8：voiceSubmit 固定 `images: null`，打字传 images — 由 `submitTranscript` 的 `{ images }` 选项参数化；空 images 数组在 helper 内归一为 `null`
- ✅ 附带给 typed 路径带来 stale task 安全性（旧路径只处理 `_draft`；新路径与 voice 一致使用 `moveTaskMessages`，自动覆盖 `_draft` 或 UI 在 STT 进行中自动恢复的陈旧 task id 两种起点）
- 验证：`submit.test.js` 9 个用例覆盖两条落点（active task / `_draft`）、stale-task 迁移、相同 key 不移动、失败回退、active id 捕获时机、`images` 数组→`null` 规整

### B2. 事件注册去重
**文件:** 新建 `ui/src/lib/events.js`、改写 `ui/src/routes/+page.svelte`、`ui/src/routes/+layout.svelte`、`ui/src/routes/history/+page.svelte`、`ui/src/routes/tools/+page.svelte`

#### B2.1 `safeListen` + 注册表封装（推荐先做）
当前 `safeListen(event, handler)` 与 `let unlisteners = []` / `let unlistenTitleUpdate` 在四个页面的形态：

- `+page.svelte:604/649-656` — `let unlisteners = []` + push 闭包；`onMount` 注册 N 个，`onDestroy` 遍历反注册
- `+layout.svelte:158/160-167` — 同上逐字复制（仅 `logger.error` 的 tag 不同：`'+page'` vs `'+layout'`）
- `history/+page.svelte:46-58` — 单事件变体：`let unlistenTitleUpdate = null`，onMount 里 `listen('task:title-updated', ...)`，onDestroy 里 `unlistenTitleUpdate?.()`
- `tools/+page.svelte:17-18/40-53` — 双事件变体：`unlistenSkills` + `unlistenMcp`，各自单独 try/catch，反注册亦各自 `?.()`

四处逐字复制，差异只在 logger tag 与单一/集合形式。

**抽取目标（events.js）:**

```js
// 返回带注册/反注册能力的 handle；批处理版本接收 [{event, handler}, ...]
export function registerListeners(map, { tag = 'unknown' } = {}) {
  // map: { [event]: handler | handler[] }
  //   - 收集全部 unlisten 句柄，监听失败只 log，不抛
  //   - 返回 { dispose() }：按注册顺序反注册，再清空句柄数组
}

// 单事件便利 API（供 history / tools 现有写法用）
export function registerOne(event, handler, { tag } = {}) { ... }
```

- `+page.svelte` / `+layout.svelte` 改为 `const events = registerListeners({...}, { tag: '+page' })` + `onDestroy(() => events.dispose())`
- `history/+page.svelte` 改为 `const unlisten = registerOne('task:title-updated', handler, { tag: 'history' })` + `onDestroy(() => unlisten.dispose())`
- `tools/+page.svelte` 同样收敛到 `registerOne` 两次（保持双字段反注册形态）

⚠️ `safeListen` 现有的"失败仅 log 不抛"语义必须保留——`+page.svelte` 包了一层 try/catch（`onMount` 内 `try{ await safeListen... } catch`），目的是不让注册失败阻塞组件挂载。helper 内置同样语义即可。

#### B2.2 事件重复订阅清单
下列事件被 2-3 个模块订阅，至少评估每条是否真需要并行：

| 事件 | +page | +layout | history | tools | 处置 |
|------|-------|---------|---------|-------|------|
| `task:created` | ✗ | ✓（notify + waiting） | ✗ | ✗ | 仅 layout |
| `task:completed` | ✗ | ✓（notify + ready） | ✗ | ✗ | 仅 layout |
| `task:error` | ✓（banner + loadTasks） | ✓（notify + ready） | ✗ | ✗ | 两处必要（前端 banner 仅 +page；notify 仅 layout）— **保留** |
| `task:updated` | ✓（banner + handleTaskUpdated） | ✓（notify + waiting/ready 切换） | ✗ | ✗ | 两处必要（同上）— **保留** |
| `task:title-updated` | ✓（tasks[idx].title = …） | ✗ | ✓（独立字段更新） | ✗ | 两处独立 setState，收归 helper 后改 `updateTaskTitle(taskId, title)` 供两处复用 |
| `hotkey:rebind` | ✓（hotkeyBinding） | ✓（hotkeyBinding） | ✗ | ✗ | **合并**——下放为 hotkey store，handler 内部 `hotkeyStore.set(new_binding)`；两页订阅 store 即可 |
| `mcp:status_change` | ✗ | ✓（notify） | ✗ | ✓（重新拉取） | 两处必要 |
| `skills:status_change` | ✗ | ✓（no-op 占位） | ✗ | ✓（重新拉取） | layout 当前是空回调，删除该订阅即可 |

#### B2.3 `hotkey:rebind` 合并（最高优先）
现状 +page.svelte:836-841 与 +layout.svelte:307-312 几乎逐字：

```
const data = event.payload || {};
if (data.new_binding) {
  hotkeyBinding = data.new_binding;
}
```

差异只在赋值对象不同（各自 `hotkeyBinding` $state）。

**方案**: 在 `stores.js` 新增 writable `hotkeyBindingStore`，初始化为 `'Ctrl+Shift+Space'`，订阅方 `$state` 改用 `hotkeyBindingStore.subscribe`（已有先例：`recordingOverlay.subscribe((v) => overlay = v)`）。或者更简单：把 handler 收敛到 events.js 一个共享工厂 `{ event: 'hotkey:rebind', apply: (data) => hotkeyBindingStore.set(data.new_binding) }`。

⚠️ 分歧风险 #9：现两处都使用 `data.new_binding` 守卫（事件可能携带 `{ old_binding }` 而无 new）。合并后必须保留该守卫，否则会把空值写入全局 store，污染初始显示。

#### B2.4 `task:title-updated` helper
+page.svelte:831-835 与 history/+page.svelte:51-54 各做一次 find-and-replace title（前者通过 store tasks 数组的局部更新，后者局部 tasks 数组）。改写后：

```js
// stores.js
export function updateTaskTitle(taskId, title) {
  tasksStore.update(($tasks) =>
    $tasks.map((t) => (t.id === taskId ? { ...t, title } : t))
  );
}
```

但注意两处 `tasks` 不是同一个 store（一个是当前任务列表的预览，另一个是分页历史）。最简方案：保留两个独立数组，但在 events.js 暴露一个 `{ event: 'task:title-updated', apply: ({task_id, title}) => { updateCurrentTasksTitle(task_id, title); updateHistoryTasksTitle(task_id, title); } }`，或在每个页面 `onMount` 内调一次 `registerOne`。

#### 验证
- 单元测试 `events.js` 新增：用 jsdom mock `listen` 验证注册成功 + dispose 调用顺序 + 失败不抛
- 手动验证四页行为不变（特别关注通知类 toast 应正常显示，history 页面任务标题应当即时刷新）

### B3. 热路径 helper（streaming.js）
**文件:** `ui/src/lib/streaming.js`、`ui/src/routes/+page.svelte`

`+page.svelte` 的 chunk/event 处理器直接拼装 stepId / toolId / step 消息字面量，导致同一种 id 格式散落 8+ 处、工具消息 builder 重复两份。

#### B3.1 id 工厂（必做）
当前 8 处模板字符串（已验证）：

| 位置 | 模板 | 用途 |
|------|------|------|
| `+page.svelte:845` | `thought-${tid}-${step}-${run}` | agent:thought handler |
| `+page.svelte:846` | `reasoning-${tid}-${step}-${run}` | 同上 |
| `+page.svelte:864` | `${stepIdPrefix}-${tid}-${step}-${run}` | listenChunk 通用 |
| `+page.svelte:875` | `reasoning-${tid}-${step}-${run}` | listenChunk（thought 分支） |
| `+page.svelte:930` | `tool-${tid}-${step}-${run}-${callIdOrName}` | agent:action |
| `+page.svelte:931` | `reasoning-${tid}-${step}-${run}` | agent:action |
| `+page.svelte:932` | `thought-${tid}-${step}-${run}` | agent:action |
| `+page.svelte:967` | `tool-${tid}-${step}-${run}-${callIdOrName}` | agent:observation |

每处都写 `data.run_id ?? 0`，一处失守即产生重复 step（导致 streaming 累积错乱）。

**抽取到 `streaming.js`**:

```js
export const stepId = (prefix, taskId, stepNumber, runId) =>
  `${prefix}-${taskId}-${stepNumber}-${runId ?? 0}`;

export const toolId = (taskId, stepNumber, runId, callIdOrName) =>
  `${stepId('tool', taskId, stepNumber, runId)}-${callIdOrName}`;
```

替换 8 处后，保证 `agent:thought` / `agent:action` / `agent:observation` 三处对同一 step 算出完全一致的 id。

⚠️ 替换前后必须跑 `streaming.test.js` 的 `applyThoughtSnap` 案例——它直接比对 segment ids；任何一处 off-by-one（多余字段、缺 `?? 0`）都会让回归测试失败。

#### B3.2 `finalizeStreamBlocks` 下放 streaming.js
当前 `+page.svelte:29-35` 定义：

```js
function finalizeStreamBlocks(messages, reasoningId, thoughtId) {
  return messages.map((x) =>
    (x.id === reasoningId || x.id === thoughtId || x.id.startsWith(thoughtId + '-'))
      ? { ...x, streaming: false, segmented: false }
      : x
  );
}
```

被 `+page.svelte:938 / 946` 调用（agent:action 静默/非静默两分支）。

**步骤**:
1. 把函数移到 `streaming.js`（保持纯函数，输入/输出 arrays），从 streaming.js export
2. +page.svelte:29-35 删除定义，import 改 `import { accumulateStreamChunk, applyThoughtSnap, finalizeStreamBlocks } from '$lib/streaming.js'`
3. streaming.test.js 加新 case：相同 step 下 reasoning + thought + thought-0 三个 segment 全部 final；其它 message 不动；reasoning 缺失不影响 thought 命中

⚠️ 当前的 silent / 非 silent 分支共享该函数（已合并，见审查修复 #4），无需再处理。

#### B3.3 agent:action 与 agent:observation 的工具消息 builder
两处 builder 字段高度重叠但不完全一致：

- `+page.svelte:949-959`（action）：type 固定 `'tool'`，`content: ''`，`time` 戳，`streaming: true`
- `+page.svelte:971-981`（observation）：`type: isAsk ? 'ask' : 'tool'`，`content: data.observation`，`stepNumber` 同 step，无 `time`/`streaming` 字段（observation 已 final）

差异即是语义差异（创建占位 vs. 填充实测），合并为：

```js
// streaming.js
export function newToolMessage({ id, stepNumber, toolName, time, content = '', streaming = false, askOptions = null }) {
  const isAsk = toolName === 'ask';
  return {
    id,
    role: 'assistant',
    content,
    toolName,
    type: isAsk ? 'ask' : 'tool',
    voice: false,
    stepNumber,
    time,
    streaming,
    ...(isAsk && askOptions ? { options: askOptions, awaiting: true } : {}),
  };
}
```

调用：

- agent:action：`newToolMessage({ id: toolId, stepNumber, toolName: data.tool_name, time: now, streaming: true })`
- agent:observation：`newToolMessage({ id: toolId, stepNumber, toolName: data.tool_name, content: data.observation, askOptions: data.ask_options })`

⚠️ 分歧风险 #11 关联：observation 对 `isAsk` 的判定将影响 `clearAskAwaiting` / `handleQuickReply` 的 `awaiting` 字段位置。当前两处 builder 对 `ask` 的 `options`/`awaiting` 字段处理方式不同（action 完全不设；observation 用 spread `...(isAsk ? {...} : {})`）。合并 builder 时必须遵守 observation 的处理（仅 ask 设 options+awaiting；action 创建时不需要）。新建 builder 默认不设 options/awaiting，ask 单独通过 `askOptions` 参数传入。

⚠️ 区分 `streaming` 默认值：action 想要 `streaming: true`，observation 想要 `streaming: false`。参数默认 `false`，action 处显式传 `true`。

⚠️ 注意 `agent:observation` 当前 handler 982-989 行：找到现有 message 时 `{ ...next[idx], ...msg, streaming: false }`——这里 `msg.streaming=false` 显式覆盖 idx 处的 `streaming=true`（由 action 创建），合并 builder 后行为保持一致。

#### B3.4 测试覆盖
- `streaming.test.js` 新增 `stepId` / `toolId` 案例：`runId` 缺失时与 `runId=0` 同结果；前缀顺序不变
- `streaming.test.js` 新增 `newToolMessage` 案例：tool / ask 两种工具名产出正确 type 与可选字段
- 现有 `applyThoughtSnap` / `accumulateStreamChunk` 测试必须保持原样（验证迁移正确）

#### 验证
- `npm run test:run` + `npm run check`
- 手动：开一个长 step，观察 reasoning/thought/tool 三段 id 一致 + chunk 累积无错位 + silent 工具仍正确 finalize

### B4. 小项
**文件:** `ui/src/routes/+layout.svelte`、`ui/src/lib/reviewMessages.js`、`ui/src/lib/stores.js`、`ui/src/lib/TaskCard.svelte`、`ui/src/routes/history/+page.svelte`、`ui/src/routes/+page.svelte`

收集剩余 6 个去重点，每个独立子节、实施 + 验证可控。优先做 B4.1（最高频、`resetOverlay` 路径已沉淀）和 B4.3（两个 helper 已存在，机械合并）。

#### B4.1 `resetOverlay()` helper（必做，最高频）
**文件:** `ui/src/routes/+layout.svelte:108-110, 128-135, 144-151, 221-228, 250-257, 261-268, 275-282, 290-297`

`setOverlay` 是行级便利（只做 spread merge），但**录音/转写结束**这套收尾（`visible:false + isRecording:false + processing:false + reason:null/'muted'/...` + 可选 `stopTimer()`）逐字复制 7 处：

| 位置 | 触发 | reason | stopTimer |
|------|------|--------|-----------|
| `:125-136` `closeOverlaySoon` | 转写结果/超时 | `null` | ✅ |
| `:138-151` `cancelRecording` | 用户取消 | `null` | ✅ |
| `:218-228` `recording:error` | 录音错误 | `null` | ✅ |
| `:229-257` `transcription:result` | 转写完成 | `null` | ✅ |
| `:258-268` `transcription:error` | 转写失败 | `null` | ✅ |
| `:269-286` `mute:changed` | 静音强制停 | `'muted'` | ✅ |
| `:287-297` `tray:status_changed` | 系统托盘静音 | `'muted'` | ✅ |

7 处全部 `{ visible: false, isRecording: false, processing: false, reason: <null|'muted'> }`，外加 `stopTimer()`。

**抽取到 +layout.svelte 顶层**：

```js
// Reset the recording overlay to its "hidden" state. Use after the user
// finishes a session, errors out, or is force-stopped by mute/tray.
function resetOverlay(reason = null) {
    setOverlay({ visible: false, isRecording: false, processing: false, reason });
    stopTimer();
}
```

- `closeOverlaySoon` (125-136)：保留 `setTimeout` 包装，body 改为 `resetOverlay()`
- `cancelRecording` (144-149)：`setOverlay({...})` + `stopTimer()` → `resetOverlay()`
- `recording:error` (221-227)：同上
- `transcription:result` (250-256)：同上（cancel 分支另说）
- `transcription:error` (261-267)：同上
- `mute:changed` (275-281)：`setOverlay({..., reason:'muted'})` + `stopTimer()` → `resetOverlay('muted')`
- `tray:status_changed` (290-296)：同上

⚠️ **分歧风险**：第 1-5 处全部用 `reason: null`，第 6-7 处用 `reason: 'muted'`。helper 用参数化 reason 即可。语义保持不变。

⚠️ `recording:stopped` handler（196-211）**不**属于这套复位块——它根据 `data.reason` 走"中间态"（`processing: true` 时不直接 visible:false），只在 `reason === 'cancel'` 时才复位（209）。该路径保持原样。

#### B4.2 `mergeLiveStreaming()` helper（必做，分歧 #12 修复）
**文件:** `ui/src/lib/reviewMessages.js`（新增 export）、`ui/src/routes/+page.svelte:520-548`、`ui/src/routes/history/+page.svelte:135-152`

`switchToTask`（+page）与 `reviewTask`（history）做的事高度重叠：

```js
// +page.svelte:524-539
updateTaskMessages(taskId, (existing) => {
    const toolSteps = new Set(
        existing.filter((m) => m.type === 'tool' && m.stepNumber != null)
            .map((m) => m.stepNumber)
    );
    const dbMessages = buildReviewMessages(result).filter(
        (m) => !(m.type === 'tool' && m.stepNumber != null && toolSteps.has(m.stepNumber))
    );
    const dbIds = new Set(dbMessages.map((m) => m.id));
    const streaming = existing.filter((m) => m.streaming);
    return [...dbMessages, ...streaming.filter((m) => !dbIds.has(m.id))];
});
```

```js
// history/+page.svelte:142-146
updateTaskMessages(task.id, (existing) => {
    const dbIds = new Set(dbMessages.map((m) => m.id));
    const streaming = existing.filter((m) => m.streaming);
    return [...dbMessages, ...streaming.filter((m) => !dbIds.has(m.id))];
});
```

共同形态：「DB messages + 还活着的 streaming 补差」。差异只在 `+page` 多了一步 tool-step 去重（运行中的 task 切走时避免重复显示）。

**抽取到 `reviewMessages.js`**：

```js
/**
 * Merge DB-loaded messages with any in-memory streaming messages that
 * arrived concurrently (e.g. a task still running while the user
 * navigates back). Streams append only when the DB doesn't already have
 * a bubble with the same id.
 *
 * @param {Array<object>} dbMessages   buildReviewMessages() result
 * @param {Array<object>} existing     current taskMessages entry
 * @param {{ dropToolSteps?: boolean }} [opts]
 *   dropToolSteps — when true, drop DB tool-step badges whose stepNumber
 *     is already represented by a live streaming tool card in `existing`.
 *     Used by switchToTask to avoid duplicate display while a task runs.
 */
export function mergeLiveStreaming(dbMessages, existing, opts = {}) {
    const { dropToolSteps = false } = opts;
    let filteredDb = dbMessages;
    if (dropToolSteps) {
        const toolSteps = new Set(
            existing.filter((m) => m.type === 'tool' && m.stepNumber != null)
                .map((m) => m.stepNumber)
        );
        filteredDb = dbMessages.filter(
            (m) => !(m.type === 'tool' && m.stepNumber != null && toolSteps.has(m.stepNumber))
        );
    }
    const dbIds = new Set(filteredDb.map((m) => m.id));
    const streaming = existing.filter((m) => m.streaming);
    return [...filteredDb, ...streaming.filter((m) => !dbIds.has(m.id))];
}
```

**调用点替换**：

- `+page.svelte:524-539` →
  ```js
  const dbMessages = buildReviewMessages(result);
  updateTaskMessages(taskId, (existing) =>
      mergeLiveStreaming(dbMessages, existing, { dropToolSteps: true })
  );
  ```

- `history/+page.svelte:142-146` →
  ```js
  const dbMessages = buildReviewMessages(result);
  updateTaskMessages(task.id, (existing) =>
      mergeLiveStreaming(dbMessages, existing)
  );
  ```

⚠️ **分歧风险 #12 解决**：`switchToTask` 有 tool-step 去重，`reviewTask` 没有——`dropToolSteps` 选项参数化。reviewTask 在切到已结束 task 时无影响（existing 中没有 streaming tool），行为保持不变；switchToTask 切到运行中 task 时仍去重。

#### B4.3 `cutIndexForStep()` helper（必做）
**文件:** `ui/src/lib/stores.js:127-144, 152-165`

`truncateTaskMessages` 与 `branchTaskMessages` 共用同一 findIndex：

```js
// stores.js:132-134
const cutIdx = list.findIndex(
    (x) => x.stepNumber != null && x.stepNumber >= targetStep && x.role !== 'user',
);
```

```js
// stores.js:157-159（逐字相同）
const cutIdx = list.findIndex(
    (x) => x.stepNumber != null && x.stepNumber >= targetStep && x.role !== 'user',
);
```

**抽取**：

```js
// Internal: find the index to cut at for truncate/branch. Skips user
// messages (they carry no stepNumber in the live view; the review
// builder assigns them the FOLLOWING assistant's stepNumber — cutting
// ON a user message would drop user input from the view even though
// the backend kept it).
function cutIndexForStep(list, targetStep) {
    return list.findIndex(
        (x) => x.stepNumber != null && x.stepNumber >= targetStep && x.role !== 'user',
    );
}
```

`truncateTaskMessages` 改用 `cutIndexForStep(list, targetStep)`，branch 同理。语义保持完全一致。

#### B4.4 `adoptDraftMessages` + `moveTaskMessages` 合并（建议做）
**文件:** `ui/src/lib/stores.js:167-197`

两处"搬家"逻辑几乎逐字：

```js
// stores.js:168-177 adoptDraftMessages
taskMessagesStore.update((m) => {
    const draft = m[DRAFT_KEY] || [];
    if (draft.length === 0) return m;
    const next = { ...m };
    next[DRAFT_KEY] = [];
    next[taskId] = [...(next[taskId] || []), ...draft];
    return next;
});

// stores.js:187-197 moveTaskMessages
taskMessagesStore.update((m) => {
    const list = m[fromTaskId] || [];
    if (list.length === 0) return m;
    const next = { ...m };
    next[fromTaskId] = [];
    next[toTaskId] = [...(next[toTaskId] || []), ...list];
    return next;
});
```

**抽出内部 helper**：

```js
// Move all messages from `fromKey` to `toKey` in a single store update.
// No-op when `fromKey` is missing, empty, or equal to `toKey`.
function _moveMessages(m, fromKey, toKey) {
    if (!fromKey || !toKey || fromKey === toKey) return m;
    const list = m[fromKey];
    if (!list || list.length === 0) return m;
    const next = { ...m };
    next[fromKey] = [];
    next[toKey] = [...(next[toKey] || []), ...list];
    return next;
}

export function adoptDraftMessages(taskId) {
    taskMessagesStore.update((m) => _moveMessages(m, DRAFT_KEY, taskId));
}

export function moveTaskMessages(fromTaskId, toTaskId) {
    taskMessagesStore.update((m) => _moveMessages(m, fromTaskId, toTaskId));
}
```

⚠️ `moveTaskMessages` 原代码显式判断 `fromTaskId === toTaskId` 后 return；新 helper 内置相同守卫。B1 已依赖 `moveTaskMessages` 的语义（typed 与 voice 两路），语义保持不变。

⚠️ `_moveMessages` 标 `_` 前缀表示内部使用，不导出。

#### B4.5 共享 `taskStatusStyle()`（建议做，分歧收敛）
**文件:** `ui/src/lib/TaskCard.svelte:5-15`、`ui/src/routes/history/+page.svelte:124-133`

两套 status→样式映射：

```js
// TaskCard.svelte:5-15
const map = {
    pending: '#666',
    running: '#44cc44',
    paused: '#ccaa44',
    completed: '#4488ff',
    failed: '#ff4444',
    error: '#ff4444',
};

// history/+page.svelte:124-133
const map = {
    completed: 'success',
    failed: 'error',
    error: 'error',
    running: 'primary',
    paused: 'warning',
};
```

词汇集不一致：`TaskCard` 包含 `pending`/`paused_pending`，`history` 没有。同一状态在两处用不同 token（`pending → '#666'` vs `pending → 'default'`）。

**抽取到新文件 `ui/src/lib/taskStatus.js`**：

```js
// Canonical task status vocabulary + UI style mapping. The backend
// emits these strings via TaskStatus::as_str(); see crates/task/src/lib.rs.
//
// statusColor() returns a hex color for inline badges (TaskCard dot).
// statusVariant() returns a MaterialBadge variant for the history page.

export const TASK_STATUSES = ['pending', 'running', 'paused', 'completed', 'failed', 'error'];

const COLOR_MAP = {
    pending: '#666',
    running: '#44cc44',
    paused: '#ccaa44',
    completed: '#4488ff',
    failed: '#ff4444',
    error: '#ff4444',
};

const VARIANT_MAP = {
    pending: 'default',
    running: 'primary',
    paused: 'warning',
    completed: 'success',
    failed: 'error',
    error: 'error',
};

export function statusColor(status) {
    return COLOR_MAP[status] || '#666';
}

export function statusVariant(status) {
    return VARIANT_MAP[status] || 'default';
}
```

**调用点替换**：
- `TaskCard.svelte:5-15` → `import { statusColor } from '$lib/taskStatus.js';` 删除本地函数
- `history/+page.svelte:124-133` → `import { statusVariant } from '$lib/taskStatus.js';` 删除本地函数

⚠️ **分歧风险**：当前 `TaskCard` 的 `pending → '#666'` 与 `history` 的 `pending → 'default'` 实际是同一意图（灰色），统一为 `VARIANT_MAP.pending = 'default'` 后 `history` 行为不变；`TaskCard` 仍用 `COLOR_MAP.pending = '#666'`，行为不变。

⚠️ `paused_pending` 是历史遗留状态（见 `TaskCard.svelte:20` 的 `durationStr` 守卫）。不在 `TASK_STATUSES` 中——两个映射表都 fallback 到 default。helper 不引入新行为。

#### B4.6 `syncStore()` helper（建议做）
**文件:** `ui/src/routes/+layout.svelte:22, 47-50, 96`、`ui/src/routes/+page.svelte:45-48, 82-87, 90-96, 118-121, 621-624`

Svelte 5 `$state` 不会自动跟踪 `get(store)`，需要手动 `.subscribe()` 镜像同步。当前 5+ 处复制：

```js
// +layout.svelte:96
recordingOverlay.subscribe((v) => (overlay = v));

// +page.svelte:46-48
$effect(() => {
    const unsub = recordingOverlay.subscribe((v) => { recordingState = v; });
    return unsub;
});

// +page.svelte:118-121
$effect(() => {
    const unsub = modelStateStore.subscribe((v) => { modelState = v; });
    return unsub;
});

// +page.svelte:621-624
$effect(() => {
    taskMessagesDict = get(taskMessagesStore);
    const unsub = taskMessagesStore.subscribe((v) => { taskMessagesDict = v; });
    return unsub;
});
```

**抽取到新文件 `ui/src/lib/syncStore.js`**：

```js
/**
 * Bridge a Svelte writable store into a `$state` variable. `$state`
 * doesn't track `get(store)` automatically — components must subscribe
 * to receive updates. This helper returns the unsubscribe function so
 * the caller can wire it into `$effect`'s teardown.
 *
 *   let mirror = $state(initial);
 *   $effect(() => syncStore(myStore, (v) => (mirror = v)));
 *
 * For convenience, `syncStoreImmediate` also assigns the current value
 * synchronously (some components need to seed from `get(store)` before
 * subscription fires — see taskMessagesStore mirror).
 */
export function syncStore(store, apply) {
    return store.subscribe(apply);
}

export function syncStoreImmediate(store, apply, getCurrent) {
    if (getCurrent) apply(getCurrent());
    return store.subscribe(apply);
}
```

**调用点替换**（仅示意 +page.svelte 一处）：

```js
// before
$effect(() => {
    const unsub = modelStateStore.subscribe((v) => { modelState = v; });
    return unsub;
});

// after
$effect(() => syncStore(modelStateStore, (v) => (modelState = v)));
```

⚠️ `+page.svelte:621-624` 的 `taskMessagesDict` 需要立即赋值（先 `get()`，再订阅），所以用 `syncStoreImmediate(taskMessagesStore, ..., get)`。其它都是单纯订阅，用 `syncStore`。

⚠️ `+layout.svelte:22` 的 `themeStore.subscribe((v) => theme = v.theme)` 略特殊（订阅但只取 `v.theme` 字段）。可以直接调用 helper：`syncStore(themeStore, (v) => (theme = v.theme))`。语义保持。

#### 验证
- `npm run test:run` + `npm run check`
- 单测新增：
  - `reviewMessages.test.js` 增加 `mergeLiveStreaming` 案例：空 existing、streaming 全保留、streaming 中 id 已在 db 的丢弃、`dropToolSteps: true` 时按 stepNumber 去重
  - `stores.test.js` 增加 `cutIndexForStep` 边界：targetStep 小于所有 / 大于所有 / 落在 user 上
  - `taskStatus.test.js`（新建）：覆盖每个 status 的 color + variant，未知 status fallback
- 手动验证：
  - 录音 → 取消 / 静音强制停 / 转写完成 / 转写失败 → overlay 都正确复位
  - history 页点 task → 进 chat 显示完整 DB 消息 + 残留 streaming 正确合并
  - 切到一个仍在运行的并行 task → 无重复 tool badge
  - 暂停、关闭、再回 history review → 用户消息位置未变化

---

## 分歧风险清单（重构时需一并处理）

| # | 位置 | 分歧 |
|---|------|------|
| 1 | react.rs:316 vs 553/577 | 主错误路径用 `emit_error`（清理 balanced_model_notified + cumulative_usage），compaction 两条错误路径直接 `emit_task_error_from` 跳过清理 |
| 2 | react.rs:1099-1108 | 有 history step 时存 display_observation，无 step 时存原始 JSON step_result，同块内不一致 |
| 3 | react.rs:182-261 | background-job 结果不发 Supplement 事件 + 不建 thought step，reload 后无可回滚对照 |
| 4 | react.rs:1227-1243 vs lib.rs:166-183 | `message_window_size` 与 `conversation_window_size` 同源可漂移 |
| 5 | lib.rs:59-69 vs react.rs:917-926 | 截断文案不同（`[... {} chars omitted]` vs `[... truncated {} chars omitted]`），字节数 vs 字符数 |
| 6 | app-binary lib.rs:222-284 | `"task:updated"` 三种载荷形状（TaskCompleted / TaskError / TaskUpdated 各发一份）——实测前端无任何消费者读 `task:updated` 的 `error` 字段，三条形状可统一为 `{task_id, status, title}`，删 `error` 零影响（详见 A9 阶段 1） |
| ~~7~~ | ~~前端 B1~~ | ✅ 已解决（B1）：打字失败保留气泡 + toast ↔ 语音失败移除气泡 + rethrow — 已统一为"失败时移除气泡 + rethrow" |
| ~~8~~ | ~~前端 B1~~ | ✅ 已解决（B1）：voiceSubmit 固定 `images: null` ↔ 打字路径传 images — 由 `submitTranscript({ images, voice })` 统一参数化 |
| 9 | 前端 B2 | `hotkey:rebind` 两处仅守卫差异，改动需双份 |
| 10 | 前端 B3 | streaming.js `applyThoughtSnap` 理解增量恢复，+page 内联 finalize 盲目清 streaming，两者语义可能漂移 |
| 11 | 前端 B4 | `clearAskAwaiting` 清全部 ask 卡片，`handleQuickReply` 只清单卡，task:updated 监听用宽版导致未答复卡片丢失提示 |
| ~~12~~ | ~~前端 B4.2~~ | ✅ 已解决：`switchToTask` 有 tool-step 去重 ↔ `reviewTask` 没有 — 抽 `mergeLiveStreaming(dbMessages, existing, { dropToolSteps })` 后两路共用，switchToTask 传 `dropToolSteps: true`，reviewTask 用默认 false |
| 13 | 时间格式 | 实时消息 `toLocaleTimeString()`，reload 消息 `YYYY/MM/DD HH:MM:SS`，同一 ChatBubble 两种格式 |

---

## 执行顺序与验证

1. 阶段 A1-A3（纯后端、有测试覆盖）→ `cargo test -p haven-agent -p haven-common` + `cargo clippy -- -D warnings`
2. 阶段 A4-A8 → `cargo test --workspace` + `cargo clippy -- -D warnings`
3. ~~阶段 B1-B3~~ → `npm run test:run`（在 ui/）+ `npm run check` — B1 已完成（2026-08-05），B2/B3 计划已细化待实施
4. 阶段 B4 + 收尾 → `/test` `/test-ui --run` `/check`
   - B4.1 `resetOverlay()` + B4.3 `cutIndexForStep()` 先做（机械、最有把握）
   - B4.2 `mergeLiveStreaming()` 必做（分歧 #12 修复）
   - B4.4 `_moveMessages()` + B4.5 `taskStatus.js` 一次性收尾
   - B4.6 `syncStore()` 最后做（最广，但只是机械替换）
5. ~~A9 单独评估~~ ✅ 已完成（2026-08-05）：细化 plan + 实施一次落地，见 A9 节末尾"实施记录"

每步小步提交验证，避免一次大改后难以定位问题。实施时行号以实际文件为准。
