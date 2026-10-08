# ADR 0810：确认批次恢复计划与结果原子清理

## 状态

已采纳并实施（2026-10-08）。

## 背景

一次模型响应可能同时包含安全工具和需要人工确认的工具。此前实现把整个批次都伪装成 `InteractionRequest`，并把无需确认的项标为 `Resolved`。SessionActor 只接受 Pending 确认，因此混合批次在登记时失败，日志出现 `confirmation batch contains a non-pending confirmation`。

确认执行完成后，工具结果和 `interaction_cleared` 又分两次提交。两次写入之间崩溃会恢复出仍可重放的已批准请求；继续运行还可能再次执行已经成功的副作用。另有三个相邻缺口：Error 状态 Continue 没有清旧请求；resume 校验压缩调用数组后错用原 provider 索引；工具步骤已经是终态时，执行入口忽略 `start_tool_step` 的 `false` 并继续执行。

## 决定

1. 新增 Agent-owned `confirmation_batch_planned` 事件，记录批次 step、工具顺序与身份（`step_id`、原始 `tool_index`、`tool_call_id`、可选的真实 confirmation request ID）。它不复制工具参数或授权 receipt；恢复时从同一 step 的 canonical ToolCall transcript 取参数。无 request ID 的安全兄弟仍留在有序计划中，不进入交互 owner registry。
2. SessionActor 在一个 SQLite 事务中追加真实 pending 请求、批次计划事件并将 session 改为 Paused。计划校验要求每个真实请求唯一匹配一个工具身份。
3. 批次最后一个 ToolResult 与清除该批次 confirmation IDs 的 `interaction_cleared` 事件，在一次 `SessionCommitted` 事务中追加。只有事务成功后才从 actor 的内存 registry 移除请求。恢复计划只在其所有真实 request IDs 被清除时关闭。
4. Continue 从 Paused 或 Error 状态开始时都持久清理旧交互。`ensure_and_start_tool_step` 返回 `false` 表示步骤已是终态；执行入口将其作为错误处理，不再重放副作用。
5. Resume 输入校验使用计划携带的原始 provider `tool_index`，即使中间过滤了 `final_answer`，校验失败仍匹配正确工具。

## 影响与替代方案

- `session_events` 增加一个 Agent domain event 类型；数据库 schema、IPC 与 UI wire contract 不变。现有事件流没有该事件时，旧式全确认批次仍可从已有确认请求恢复。
- 计划事件只保存调用身份，不重复存储参数、receipt 或授权决定。工具调用参数仍以 canonical ToolCall transcript 为准。
- 继续接受合成 Resolved request 会违反 ADR 0424 的 pending owner 不变量；结果后再单独 append clear 仍保留崩溃重放窗口，因此不采用。
- 不要求重置数据库：新版本可读取既有事件流，新增事件只由本版本的新确认批次写入。回退到不识别此事件的旧二进制时，不支持继续一个新版本创建的混合确认批次；正常升级与当前版本重启不受影响。

## 验收

- 混合批次集成测试验证只登记真实 gate、确认前不执行兄弟、批准后完整有序批次继续运行。
- SessionActor 故障注入验证计划事件 append 失败时，请求、Paused 状态与计划均不部分提交。
- Memory 故障注入验证最后 ToolResult 与 clear event 同事务回滚或提交。
- 其他回归覆盖错误 Continue 清理、最终步骤禁止重复执行、计划事件 replay 和 final-answer 间隙后的原始索引。
- 变更范围跨 Agent 与 Memory 持久化恢复，执行 workspace 编译、严格 Clippy、测试及格式检查。

## 回滚

代码回滚不改变 schema。若需运行不识别 `confirmation_batch_planned` 的旧二进制，先确保没有由本 ADR 版本创建且尚未完成的混合确认批次；不删除或重写 append-only 历史事件。
