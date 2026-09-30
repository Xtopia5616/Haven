# ADR 0416：持久跟踪待投递的会话输入

- 状态：已采纳
- 日期：2026-09-30
- 关联：[ADR 0207](0207-session-store-replay-boundaries-and-durable-ui-sequences.md)、[ADR 0336](0336-react-session-committed-submission.md)；取代 [ADR 0238](0238-session-recovery-read-ports.md) 的恢复扫描契约

## 背景

resume/reopen 会重新排队尚未进入 transcript event 的用户补充输入。此前这类消息靠“没有 thought step 且创建时间在最近两天”的查询识别。进程停机超过两天后，消息仍在历史中，但无法恢复投递；时间戳也不应决定一个已接受输入是否仍待投递。

## 决定

1. schema v32 新增 `pending_session_inputs`，以 message ID 持久记录已接受、进入现有 session 队列的用户输入。创建用户消息与 marker 在同一 SQLite 事务提交；重复的稳定 message ID 不会重新创建已确认的 marker。
2. 对应 `UserInject` 与 `AcknowledgePendingUserInput` 在同一个 `SessionCommitted` 事务提交。投影失败会回滚 event 和 ack；进程在提交前退出时 marker 保留，提交后退出时 event 已可回放且 marker 已清除。
3. resume 和 history reopen 只按 marker 恢复输入，不使用时间窗口、message 内容去重、thought-step 缺失推断或并行 cursor-after 扫描。待投递结果按既有 `ingress_seq` 排序。
4. message/session 删除通过外键级联清除 marker。terminal ingress 明确拒绝输入时也会清除 marker，即使 best-effort 消息删除失败。marker 数量最多对应仍存在且尚未投递确认的已接受输入，不复制消息正文或附件。

## 影响与验证

任意时长停机都不会让 pending 输入因年龄而丢失；投递确认与 durable event 原子化，避免提交边界上的重复投递或丢失。新增 schema 表，不改变 Tauri IPC。

验证覆盖：超过旧两天窗口仍可恢复、未标记历史消息不恢复、相同内容不同 ID 不折叠、event 投影失败时 marker 保留、成功提交后 marker 删除、terminal ghost 删除失败后的 marker 清理，以及 Agent reopen/resume 的恢复顺序。

## 兼容与重置

本项目不运行时迁移旧 schema。升级到 v32 前退出 Haven，并按 [发布与数据重置说明](../release-and-reset.md) 删除 `haven.db`、`haven.db-wal` 和 `haven.db-shm`；数据库重建会清除会话、记忆、任务和用量。

## 替代方案

- 无限期扫描所有无 thought-step 的用户消息：拒绝。旧消息、旧 ID 和失败的历史投影会被重新解释为新输入，无法明确区分“待投递”与“已处理”。
- 延长或配置时间窗口：拒绝。任意固定窗口都无法满足任意时长宕机恢复。
- 只保留 ingress cursor：拒绝。cursor 是消息顺序元数据，不能标记此前已入队但尚未进入 event 的消息，也不能区分已投递与仍待投递的历史行。

## 回滚

回滚实现时一并移除 pending marker 的写入、ack、读取和 schema 表，并恢复 ADR 0238 的时间窗查询。代码回滚前不得仅删除 marker 表，否则未投递输入将失去恢复依据；旧数据库仍按发布说明重置。
