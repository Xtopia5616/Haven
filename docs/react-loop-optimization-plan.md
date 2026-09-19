# ReAct Loop 性能与实现重构计划

## 目标

在不破坏 `session_events` X12 事件权威、canonical 顺序、确认/取消/恢复语义和前端事件契约的前提下，降低长会话的每轮 CPU/内存开销、SQLite 写放大、工具批次尾延迟和 UI 流式重渲染成本。

本计划拆成 7 个逻辑批次。每个批次完成后单独验证，避免把 prompt 投影、上下文队列、工具身份、持久化和 UI 事件问题混在一次重构中。

## 不变量

- `session_events` 仍是 append-only 权威；`messages`、`session_steps` 和 snapshot 仍是投影/缓存。
- canonical transcript 仍按 provider tool-call 顺序物化，工具实际完成顺序不能影响下一轮 prompt。
- inbox 只有在 transcript 投影和 snapshot durable 后才 ack。
- 工具的 `step_id`、`action_index`、`tool_call_id` 继续保持稳定关联。
- 取消、确认恢复、未知副作用、崩溃恢复、rollback 和 at-least-once inbox 重投语义不变。
- 新增缓存、队列或后台 writer 必须具备容量上限、取消行为、并发模型和压力/边界测试。

## 批次 1：性能基线与观测

### 范围

- 增加 ReAct 阶段耗时和计数指标：turn-start、context inject、token estimate、RequestContext、首 token、完整 LLM stream、tool admission、tool execution、ordered commit、event append、projection、snapshot、SQLite lock wait。
- 增加队列长度、chunk drop、checkpoint pending 和 UI frame/chunk 计数。
- 为指标添加 session/run/step 关联，但不得记录完整用户内容、密钥或完整工具输出。

### 验收

- 能区分 p50/p95 的模型等待、数据库等待和本地 CPU 时间。
- 指标不会阻塞 ReAct 主路径。
- 不改变业务行为和持久化格式。

## 批次 2：Prompt 热路径

### 范围

- 给 `ReActState.canonical` 引入 revision/generation。
- 让 token estimate cache 以 revision 为主要键，移除每轮对完整 canonical 的 fingerprint 扫描。
- 给 `RequestContext` 增加 canonical/media projection 的 fast path 和必要缓存。
- 无媒体输入时不构建 media index；provider retry 不重复 clone 完整 messages/tools。

### 验收

- rollback、compaction、resume、retry 后 revision 一致且不会错误命中缓存。
- 长 canonical 下 token estimate 和 RequestContext 的本地 CPU/分配次数下降。
- provider 请求内容与重构前逐项一致。

## 批次 3：上下文队列与批量注入

### 范围

- 为 steering、follow-up、action result 和 inbox 增加 item/字符/附件字节上限。
- 超限时显式保留或延后处理，不静默丢用户输入。
- 增加 `apply_transcript_batch` 或等价批量注入路径。
- 避免每次注入都扫描完整 `state.events` 构建去重集合。
- 保持 steering 优先级和 inbox claim/ack 边界。

### 验收

- 高并发输入不会无界增长内存或单轮 prompt。
- 批量注入的 event sequence、message_id、step_id 和 UI 顺序与逐条处理一致。
- snapshot 失败时 inbox 不 ack，并能下一轮重投。

## 批次 4：ToolBatch 准入与身份契约

### 范围

- 每个 Turn 获取一次 session tool catalog snapshot。
- 用 snapshot 批量完成工具查找、schema validation、权限/并发/幂等/renderer metadata 读取。
- 明确 `action_index` 是 provider 原始数组位置还是过滤后的紧凑位置；优先保持 provider 原始稳定位置。
- 更新确认恢复、action card、observation、replay 和相关测试。

### 验收

- N 个工具调用不再进行 N 次串行 catalog lookup。
- 无效参数仍以结构化失败 observation 返回，不修改原始输入。
- 确认暂停/恢复后所有三元身份完全一致。

## 批次 5：Transcript 与数据库批量持久化

### 范围

- 抽取 live `TranscriptBatchWriter`，复用 `SessionEventStore.append_batch`。
- ToolCall、action step、ToolResult、messages、session_steps 尽量按一个批次写入。
- 事件、投影和 live emit 保持明确顺序；durable commit 后才修改内存 canonical 和发送权威 UI 事件。
- 优化无 active partial 时的 `partials.discard` fast path。
- 评估 branch point 与批次 snapshot 的写入合并边界。

### 验收

- 多工具批次的 SQLite transaction、`run_blocking` 和连接池等待次数明显下降。
- 事件 authority、投影修复和崩溃恢复测试通过。
- ToolBatch 的 canonical 顺序、usage、confirm、ask、cancel 和 unknown outcome 语义不变。

### 额外要求

- 该批次属于跨 crate 持久化边界变更，必须补充或更新 ADR，说明事务边界、失败恢复和回滚策略。

## 批次 6：流式尾延迟与 UI 批量更新

### 范围

- 评估 checkpoint writer/barrier，避免普通 stream flush 等待多个独立 checkpoint task。
- 保留 generation 丢弃晚到 checkpoint 的安全语义。
- 前端增加一帧一个 `agent/chunks` reducer action，避免每个 chunk 都复制完整 message list。
- 对 `loadSessions` 生命周期刷新做 debounce/coalesce。

### 验收

- 最终文本、reasoning reconcile、stream reset 和 chunk 丢失修复行为不变。
- UI frame 更新次数和 message array 复制次数下降。
- session completed/error/paused 时不存在晚到 chunk 重开 streaming bubble。

## 批次 7：集成验收、压力测试与最终审查

### 场景

- 空响应、普通文本、reasoning、web search、单工具和 64 工具批次。
- 工具并发、工具取消、超时、确认暂停/恢复、未知副作用。
- 长会话、频繁 steering、inbox burst、跨 session 消息和 at-least-once 重投。
- provider retry、stream reset、checkpoint 延迟、进程中断恢复。
- rollback、compaction、resume、UI reconnect/replay。

### 验收

- Rust workspace test、严格 Clippy、UI check、UI test、生产构建全部通过。
- 关键 p95 指标相较基线有可解释的改善；若无改善，保留数据并回退无收益改动。
- `git diff --check` 和 staged diff 复核通过。
- 不包含密钥、用户数据、生成产物或与本计划无关的已有修改。
- 最终由主 agent 做一次架构、契约、恢复语义和性能结果审查后提交，不自动推送。

## 推荐执行顺序

```text
1 基线
├── 2 Prompt 热路径
├── 3 上下文队列
└── 4 ToolBatch 契约
    └── 5 批量持久化
        └── 6 流式/UI
            └── 7 集成验收与提交
```
