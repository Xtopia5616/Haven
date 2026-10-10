# 0913：完成 SessionStore 生产读取的异步边界

## 状态

已接受并实现（2026-10-11）。

## 背景

ADR 0909 将 SessionStore 的同步 SQLite 原语收回 Memory，但调用图复核发现遗漏了仍可被生产调用的 `session_record` 与 `pending_session_records`。Agent 生命周期和 dispatcher 都在 async 路径调用它们；App 通知事件处理则经同步 `session_record` 读取会话展示标题（ADR 0286）。`subscribe_from` 与 AgentLayer 转发仅在定义和测试中出现，没有生产消费者；将它改成 async 仍会保留无消费者的公共 API。此外，`cursor()` 仅用于 Memory 测试，却仍默认公开；单条同步 `append_usage()` 仍被 Agent 生产路径手动放进 `spawn_blocking` 调用，尽管 Memory 已有异步批量写端口。这些入口绕过或重复 blocking pool 边界；pending 恢复还无法把 shutdown cancellation 传到数据库查询。

`RunEvent::Exit` 的 `pause_running_sessions` 不同：Tauri exit 回调必须同步完成该状态更新，且暂停必须先于 blocking teardown（ADR 0287）。它是唯一保留的生产同步 SessionStore SQLite 操作。

## 决定

- 删除同步 `SessionStore::session_record`。Agent lifecycle 的按 ID 读取统一使用已有 `load_session_record`，经 Memory blocking pool 执行；缺失记录仍为 `Ok(None)`。
- 将 pending recovery 查询改为 `pending_session_records_cancellable`。Memory 保持无筛选、无分页语义并拥有 SQL；Agent 的 retry owner 传递 shutdown token，普通 `load_pending_sessions` 使用独立 token。取消同时作用于 lifecycle gate 等待、SQLite 查询和逐会话安装边界。取消发生在批次处理中时，已安装 actor 可由后续扫描识别并补入队列；重复扫描继续遵循 actor 去重规则。取消不作为数据库失败重试，也不记录为 retry warning。
- 删除没有生产消费者的 `subscribe_from`、AgentLayer 转发与 `SessionEventSubscription` 公共包装类型，不保留兼容别名。真实 live 消费仍使用 `subscribe()`；需要历史的消费方组合 live receiver 与现有 durable read port，遵循先订阅、再回放、按 `(session_id, sequence)` 去重的契约。
- 通知标题 cache miss 改为使用已有 `get_session_title_or_input` typed read。`TauriEmitter` 由单消费者 `BufferedEmitter` FIFO 驱动，因此等待 blocking-pool 查询不会重排 Tauri 生命周期事件。查询失败继续记录脱敏 warning 并回退到 session id；事件 payload 形状和队列顺序不变。空字符串标题现在与未设置标题一致，回退到输入文本。`SessionCreated` toast 仍只使用事件中的标题或 session id；仅缓存实际非空标题，避免将 id fallback 当成权威标题。
- 把同步 `memory_event_cursor` 和 `checkpoint_memory_event_cursor` helper 限为 SessionStore 私有实现；对外能力继续通过 cancellable async API 提供。
- 把无生产消费者的同步 `cursor()` 限为 Memory 单元测试可见。
- Agent ReAct usage writer 改用既有 `append_usage_batch_cancellable`；删除手工 `spawn_blocking` 与单条同步存储调用。将同步 `append_usage()` 限为 Memory/Agent 测试和 `test-support`，不改变 usage event、投影事务、计数或发布顺序。
- pending session read 在启动前检查取消状态，进入查询后由现有 interrupt handle 中断阻塞中的 SQLite 操作。保留 `run_blocking_cancellable` 对已开始/收尾写入的既有语义，不能全局拒绝已取消 token 后启动 closure，因为 Agent 的部分提交清理必须在取消边界完成 durable 收尾。
- 保留 `pause_running_sessions` 同步 typed 操作作为 Tauri exit callback 的窄例外，不增加通用同步 repository 能力。

## 影响

普通生产 session record、pending recovery 与 usage 写入均经 Memory typed async 边界运行在 blocking pool；Agent/App 不再有这些同步 SQLite 入口。测试专用 cursor/single-usage 操作不进入默认生产 API。pending recovery 会拒绝已取消后才启动的读取，并可中断已阻塞的 SQLite 查询；通用 cancellable writer 仍允许执行其取消点之前已接受的 durable 收尾。无生产消费者的 replay/live 包装 API 与其 Agent 转发已删除；Memory 仍提供实际消费者使用的 live receiver 和 durable replay ports。标题 cache miss 不再阻塞 Tauri event consumer 的 Tokio worker；FIFO、payload shape、持久化数据、event sequence 和安全策略不变。`get_session_title_or_input` 同时服务通知标题与 App 会话展示标题，空标题现在与未设置标题一致，回退到输入文本。Rust crate API 有意收口；无 schema、持久化格式或 IPC 数据结构变更，无需数据重置。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `pwsh -NoProfile -File scripts/check-adr-index.ps1`

测试覆盖 async session record 与缺失语义、pending 查询顺序/过滤及启动前取消、现有 MemoryRuntime startup replay/live 无空隙恢复、标题为空时回退输入、Agent usage 写入事务保持，以及原有 SQLite 中断和取消后 durable 收尾行为。

## 替代方案

- 保留同步 typed reads 并要求 Agent/App 自律：拒绝；调用图已证明 async 生命周期会同步阻塞 SQLite，且 API 无法传递取消。
- 将无消费者的 gap-free replay wrapper 转为 async：拒绝；保留没有当前职责消费者的公开 API没有收益，且现有 durable read/live capabilities 足以组合所需行为。
- 让通知标题每次 cache miss 返回 session id：拒绝；会改变已存在的标题→输入文本展示语义，FIFO event consumer 可以等待 typed async read。
- 把全部同步 SessionStore 能力删除：拒绝；Tauri exit 回调不能 await，且正常退出暂停会话是 crash-recovery 状态契约的一部分。

## 回滚

恢复已删除的同步 SessionStore API、单条同步 usage 调用、Agent/App 调用与 replay wrapper 即可回退；没有持久化恢复或数据重置步骤。若回滚标题读取，应同步恢复 ADR 0286 的同步缓存 miss 行为。
