# ADR 0336：ReAct transcript 以 SessionCommitted 提交

- 状态：已采纳
- 日期：2026-09-25
- 范围：`ReActEngine::apply_transcript`、`SessionStore` transcript commit 与 X12 投影顺序
- 关联：[ADR 0207](0207-session-store-replay-boundaries-and-durable-ui-sequences.md)、[ADR 0210](0210-committed-ui-sequence-publisher.md)、[ADR 0255](0255-transcript-batch-session-store-port.md)、[ADR 0274](0274-thought-step-session-store-port.md)

## 背景

ADR 0255 已让 `TranscriptBatchWriter` 只依赖 `SessionStore`，但 Agent 仍组装包含数据库投影 DTO 的 transcript batch。边界收回了 blocking 调度和取消，却没有表达“这组事件及其会话投影共同构成一次提交”。

X12 要求 `session_events` 为 append-only 恢复权威，消息/步骤是物化投影；只有事务提交后的事件才能通过 `CommittedUiPublisher` 发布。为避免 Agent 持有 storage-shaped batch，同时保留事件优先写入、原子投影和序号发布顺序，提交端口需要接收一个有界的领域提交意图。

## 决定

1. `SessionCommitted` 是 ReAct live transcript 的 Agent → Memory 提交类型。它包含 Agent 序列化的 transcript event payload，以及 `AssistantMessage`、`ThoughtStep`、`ActionStep` 等领域投影意图；调用方不再描述 `messages`/`session_steps` 的数据库行或列。
2. `SessionStore::commit_transcript` 在单个 SQLite 写事务中先分配并追加所有 event，再按既有顺序物化投影。投影失败会回滚事件和投影；成功提交后才失效消息 cache 并广播事件。缺少 session、取消、空提交和批量限额保持原语义。
3. Agent 在 Store 返回后通过 `CommittedUiPublisher` 按已提交的 event sequence 发布 live UI event，然后更新进程内 canonical。assistant Thought 的消息行与 event 同事务提交；共享 `step-*` 的 Thought 执行步骤仍是发布后的独立 SessionStore 投影。该步骤失败不撤回或重复发布 event，恢复从 durable event 修复物化步骤。
4. 保持既有领域边界：Agent 仍拥有 transcript 类型、payload 编码、event/UI 映射和 ReAct 状态；Memory 拥有 sequence 分配、事务、消息/步骤行写入、cache invalidation 与 commit 后广播。没有 schema、IPC、wire payload 或 event sequence 变化。

## 保留的例外与后续路径

`SessionCommitted` 收口 live ReAct event + projection 提交，不合并不同恢复语义的消息写入：

- ingress user seed 先写入，确保队列有稳定消息 ID；随后 `UserInject` 以该 ID 建立共享 ID 的 thought step。
- error partial 有意不进入 transcript event stream，由 recovery marker、branch point 和 `last_msg_at` 控制继续/回滚。
- terminal action-result 在没有 live loop 时只保存历史消息。
- ask/confirm waiting notice 只服务 UI，不进入 LLM transcript 或 durable event stream。
- 当时 turn-end 的防御性 search-final 路径在此前的 ToolCall event 已提交、但没有 Thought 消息时，仍可能直接物化 final message 且不新增 transcript event。该路径已由 ADR 0385 收口：消息投影现在并入承载搜索上下文的 ToolCall `SessionCommitted`。

后续新增可恢复的 ReAct 内容必须进入 `SessionCommitted` 并能从 event replay 修复；新增直接 `messages` 写入必须注明其恢复/裁剪 owner。其他 session lifecycle 和非 transcript 写入继续由对应 typed SessionStore port 承接。

## 验证

- Memory 测试验证 event 先于 assistant message 投影、事件与投影原子回滚、commit 后才广播、shared ID 以及 rollback 的 `event_cursor` / `last_msg_at` 双时钟。
- Agent 测试验证 transcript apply 与提交后 Thought step 失败时的发布/恢复行为。
- 门禁：`cargo fmt --all -- --check`、`cargo test --locked -p haven-agent`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`。

## 回滚

回退本提交的 Agent intent 组装、SessionStore 提交类型、测试与文档，再恢复旧 transcript batch API。没有数据库 schema 或持久化格式迁移，无需重置数据库。
