# ADR 0458：持久化待输入的 Ask 回答路由

- 状态：已采纳
- 日期：2026-10-04
- 关联：ADR 0416、0440

## 背景

已接受的会话输入会连同 `pending_session_inputs` 标记写入 `messages`，直到对应的 `UserInject` event 提交。该标记原来只保存会话和消息身份，没有记录 ingress 当时决定的 Answer / FollowUp 路由。进程重启后，交互 gate 可能已变化；例如 Confirm 已解决而 Ask 仍挂起，按恢复时状态重新分类会把原本应当是 FollowUp 的输入误当成 Ask 回答。

Ingress 还曾在消息入队时提前清除 Ask，而不是等待对应的 `UserInject` event 持久化。如果队列或投影失败，问题可能已经被清除，但回答尚未进入 canonical transcript。

## 决定

1. 在插入用户消息的同一 SQLite 事务中，将 `disposition`（`answer` 或 `follow_up`）写入现有 `pending_session_inputs` 行。该标记是待投递输入的唯一恢复记录；消息 ID 与 `session_events` 仍分别权威决定实体身份和已提交 transcript。
2. Ingress 在持久化前读取 pending Confirm 与 Ask gate。Confirm 优先，即使 Ask 同时挂起也保存为 `follow_up`。仅 Ask 挂起时请求 `answer`，但同一会话已有未确认的 Answer reservation 时，本条保存为 `follow_up`。
3. 对每个会话最多允许一个 pending Answer reservation。部分唯一索引提供约束，插入事务会将后续 Answer 请求降级为 FollowUp，避免第一条回答提交前的重复回答路由。
4. Resume 与 reopen 按 marker 保存的路由重新入队，不根据恢复时的交互状态重算。只有对应 `UserInject` 与 marker 确认在同一事务提交后，Answer reservation 才释放；Ask 在该 durable transcript 边界后清除。提交失败时 marker 和 Ask 都保留供恢复。
5. 数据库 schema 从 v34 升至 v35。按 `docs/release-and-reset.md` 删除旧数据库及 WAL/SHM 文件，不添加运行时迁移或兼容分支。

## 替代方案

- 恢复时按 Ask/Confirm 当前状态重算路由：拒绝，因为确认状态变化会改写已接受输入的含义。
- 将路由放入独立队列或交互表：拒绝，因为会增加第二个恢复权威与双写边界。
- ingress 入队后立即清 Ask、仅依赖内存队列恢复：拒绝，因为内存队列不是 `UserInject` 的 durable acknowledgement。

## 影响与验证

待投递期间，pending marker 同时保存路由；成功确认后 marker 与路由一起删除，之后 transcript event stream 是已投递内容的唯一恢复权威。确认优先级和 Answer reservation 可跨重启保持一致。v34 数据库无法直接打开；配置可保留。

验证覆盖：Answer reservation 只授予一条待投递输入；事件提交失败时 marker 保留；成功提交后 marker 删除；缺少新列的 v35 schema 被拒绝；Ask 首答与后续 FollowUp 路由；Confirm 已解决而 Ask 仍挂起时，reopen 仍按 marker 恢复原 FollowUp 路由。验收命令：`cargo test --locked -p haven-memory`、`cargo test --locked -p haven-agent`。
