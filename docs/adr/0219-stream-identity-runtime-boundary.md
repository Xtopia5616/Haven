# ADR 0219：Stream identity 的运行时边界

- 状态：已采纳（2026-09-24）
- 范围：`haven-agent` 的 ReAct 流式消息 identity
- 关联：[ADR 0214](0214-react-run-inside-session-actor.md)

## 背景

`IdentityMap` 保存流式 thought/reasoning 气泡的进程内 identity。它只用于同一轮运行中让 stream chunk、最终 transcript 投影和 session step 复用同一 id；既不参与恢复，也不写入 durable state。此前该 map 放在 `SessionRuntimeState`，ReActEngine 的 helper 通过 `SessionActor` mailbox 往返读写它，与这项数据的生命周期和用途不符。

## 决定

1. `IdentityMap` 是进程内 sidecar，不是 durable state；其唯一 owner 是共享的 `ReActEngine`。Map 自带的 `Mutex` 负责并发访问，不增加另一层锁。
2. `ensure_msg_id`、`block_msg_id` 和 `clear_msg_ids_for_session` 直接调用 ReActEngine 持有的 map，不再查找或加载 actor，也不跨 mailbox 往返。
3. 从 `SessionRuntimeState` 移除 `stream_identity`，并删除 `EnsureStreamId`、`BlockStreamId`、`ClearStreamIds` 命令、对应 handle 方法及 actor 分支。其他内部 mailbox 命令不在本切片处理。
4. 保持 `(session, step, run, kind)` key、thought 使用 `step-`、reasoning 使用 `msg-` 的规则，以及运行开始和 `RunMsgIdGuard` 退出时按 session 清理的行为不变。该切片不改变 wire、durable event、schema 或恢复语义。
5. 这是 ADR 0214 的一个受限切片：它只收掉 stream identity 的 mailbox 往返，不表示内部 mailbox 命令已全部收口，也不表示 ADR 0214 的 run 迁移及热 transcript 目标已经完成。

## 替代方案

- 保留 map 在 actor 并继续通过 mailbox 调用：会让不持久化且由单个 ReActEngine 使用的 sidecar 依赖 session actor 生命周期，并保留无必要的内部往返。
- 把 identity 改成全局 map 或增加第二把锁：会扩大共享状态范围或重复同步；现有 ReActEngine 的共享生命周期和 `IdentityMap` 自带的 Mutex 已满足并发访问。
- 同时删除 Usage、token estimate 等内部 mailbox 命令：超出本切片边界，分别留待后续工作处理。

## 影响与验证

- session actor 不再拥有或处理 stream identity；未加载 actor 时 ReActEngine 也能同步生成、复用和清理 identity。
- ID 前缀、identity key、消息投影、事件和持久化契约保持不变；不需要数据库重置。
- engine 级测试验证 direct path 在没有 actor 时复用 thought id 并清理该 session 的映射；`identity.rs` 测试继续覆盖前缀、key 复用、session 隔离和 fallback。
- 验证命令：

  ```text
  cargo fmt -p haven-agent -- --check
  cargo check --locked -p haven-agent
  cargo test --locked -p haven-agent --lib
  cargo clippy --locked -p haven-agent -- -D warnings
  git diff --check
  ```

## 回滚

回退本切片的实现与本 ADR，即可恢复原 identity mailbox 边界；没有 schema、durable event、wire 或用户数据迁移。
