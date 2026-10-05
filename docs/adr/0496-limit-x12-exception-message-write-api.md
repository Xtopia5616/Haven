# ADR 0496：收窄 X12 例外消息写入口

## 状态

已采纳并实施（2026-10-05）。

## 背景

X12 要求正常 ReAct transcript 只经 `apply_transcript` 追加 durable event 并投影到 `messages` / `session_steps`。三类场景仍有意不写入 canonical event stream：新会话首条用户输入需先落库以支持崩溃恢复；错误时已展示的 assistant partial 需在恢复边界外保留；终态 action result 没有活动 Agent loop 可提交。

原 `SessionStore::persist_session_message` 以及 Agent 包装函数接受任意 `role`、`message_type`、附件、voice、`tool_call_id` 和 ID。它们让未来调用者可以绕过 durable event path 构造普通 transcript 行，且把三种不同生命周期契约暴露为同一宽泛能力。

## 决定

1. 删除 Agent 的通用持久消息包装和 Memory 的公开通用 writer；仅保留 `SessionStore` 内部共享行写入实现。
2. 将合法旁路表达为三个固定语义的端口：
   - `persist_ingress_user_seed` 固定为 user；消息类型由 session 已持久化的 origin 推导（普通会话 `text`，agent spawn `peer_kickoff`），并保留附件与 voice。
   - `persist_recovery_partial` 固定为 assistant，只允许 `Thought` 或 `Reasoning` 两种消息类型，不接受附件、voice 或 tool-call ID；调用者仍负责在恢复事务成功后清理 scratch partial。
   - `persist_terminal_action_result` 固定为 user/text，不接受附件、voice 或 tool-call ID；调用方先按既有策略丢弃流式 scratch partial，再沿用稳定消息 ID 保证重复投递幂等。
3. Pending input、Ask、Thought、Action、Observation、Supplement 与其它可恢复 transcript 内容仍走现有 durable event/typed projection 路径。测试用行级 helper 保持私有，不成为生产 API。
4. 保留 `add_message_full`、ID 冲突检测、`last_msg_at` 更新、阻塞池执行与既有 partial discard / recovery retry 时序；不改变 schema、消息行、事件、IPC、配置或持久化内容，因此无需用户数据重置。

## 替代方案

- 保留通用 writer 并只靠注释限制调用：拒绝。编译期接口仍允许任意 transcript 旁路，后续审查容易漏掉新调用点。
- 将三种旁路合并为一个 enum 加通用字段：拒绝。接口会继续暴露与不同语义无关的可选字段，且调用处更难看出生命周期意图。
- 强制把首条 seed、错误 partial 和终态结果都写成 canonical event：拒绝。它会改变现有启动崩溃安全、错误 partial 恢复或没有 live Agent loop 的 action 投递时序，不属于本次 API 收口。

## 影响与验证

生产调用只能选择与生命周期匹配的 `SessionStore` 方法。首条 seed 类型来自数据库中的 session provenance，而不是 Agent 调用者传入的可变标签。Recovery partial 的 kind 是封闭枚举；终态 action result 继续按稳定 ID 幂等。

回归覆盖普通与 agent-spawn seed 的类型推导、seed 附件/voice 保留、thought/reasoning partial 的固定角色与类型及重试幂等，以及 terminal action-result 的稳定 ID。完整验证按 Agent 与 Memory 跨 crate 持久化契约门禁执行：`cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo check --workspace --locked`、严格 Clippy、crate dependency inventory 与 `git diff --check`。

## 回滚

恢复通用写入函数和旧调用点可回退源码；新增 API 本身不产生新格式，已有消息无需迁移、删除或重置。回滚时仍应保持 X12 约束，并避免为普通 transcript 内容恢复无审查的直接写路径。
