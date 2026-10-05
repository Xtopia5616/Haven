# ADR 0522：Pending Session 恢复失败后有界退避重试

## 状态

已完成（2026-10-06）。

## 背景

步骤 0 复核发现，Pending session 的整批读取错误有两条启动路径，当前都只有一次机会：

- `SessionSupervisor::start_dispatcher_inner` 先设置单 dispatcher 的 `dispatcher_started`，并取走 terminal-cleanup retry receiver，再调用 `load_pending_sessions`。批次读取返回 `Err` 时 task 直接退出；同一 supervisor 不能再启动 dispatcher，后续新 Pending session 也失去自动调度。
- 桌面启动在 MCP/Skills catalog 准备后调用一次 `AgentLayer::recover_pending_sessions`。错误只记日志，bootstrap 仍进入 Ready，没有进程内重试；旧 Pending session 可能一直不进入 actor/queue。

`load_pending_sessions` 的整批查询发生在逐会话 actor 安装之前，因此顶层 `Err` 不会留下该次调用已安装一部分 actor 的情况。单个会话的 interaction replay/actor 安装错误则已被内部记录并跳过，调用仍返回成功以保留健康会话恢复；不能把 `loaded == 0` 或单条安装失败当作整批重试条件。数据库当前 schema 对 `origin` 等字段有约束，且拒绝不兼容版本；SQLite I/O、锁或完整性错误仍可能让批次查询暂时失败。

## 决定

1. `SessionSupervisor` 为完整的 `load_pending_sessions` 调用提供单一重试 owner。只有顶层 `Err` 才重试；任意 `Ok(count)`（包括 0）代表批次恢复完成，立即进入后续生命周期。每个会话内部已有的 fail-closed/跳过语义不变。
2. 失败后按 250ms 起步、指数增长、30s 封顶退避；持续失败时继续重试，直到应用取消。取消可打断退避，但不强制 drop 一次已开始的批量恢复调用，以免中断其 actor 安装副作用。
3. `RecoverImmediately` dispatcher 在进入主循环前使用该重试 owner。`DeferUntilCatalogReady` 的首次恢复也注册为 `ApplicationRuntime` 拥有的任务，bootstrap await 其结果以保持现有 Ready 顺序；该任务不因 bootstrap 的外层取消包装而 drop 正在执行的批量调用。首次整批错误后，App 再注册有退避和取消的重试任务，继续进入 bootstrap Ready。恢复成功后只记录数量，不重复触发其他生命周期入口。
4. 重试可能与 UI 对 Pending session 的显式加载交错。再次扫描时，若 actor 已存在且其当前状态仍为 Pending，则通过去重 queue 入口确保它已入队并唤醒 dispatcher；Running、Paused、终态和 closing session 不重新入队。`load_pending_sessions` 的返回数量仍只表示新安装的 actor 数量。
5. 使用 `BackoffPolicy` 作为纯退避计算来源；SQLite 访问、日志、取消、队列写入和 task 所有权仍由 Agent/App 的现有 owner 承担。

## 替代方案

- 失败后继续启动 dispatcher：会让既有 Pending session 无 actor/queue，造成静默漏恢复。
- 只重试有限次数：持续性 I/O 或锁故障超过上限后仍永久丢失本进程的自动恢复机会。
- 按单 actor replay 失败重试整个批次：会重复扫描健康会话并破坏 ADR 0520 已确立的坏记录隔离行为。
- 取消时 drop 正在运行的加载 future：恢复可能已部分写入 actor registry/queue，且 SQLite 同步查询不可被 Tokio select 抢占。

## 影响与边界

- 不修改数据库 schema、event、IPC、配置、持久数据格式或 crate 依赖方向；不需要重置数据库。
- 永久数据库错误会让旧 Pending session 保持未恢复，并使对应重试 task 以最多每 30 秒一次的频率等待，直到修复或应用关闭；错误持续可观测。
- 新鲜 session 的 dispatcher 主循环仍可在 deferred recovery 失败后照常工作；该失败不应阻塞 bootstrap Ready。
- 队列去重仍由 SessionSupervisor 持有；不会创建第二个持久恢复来源或独立重试队列。
- 实现留在现有 owner 文件：`session/status.rs` 同时拥有 pending record 读取、actor 安装与 recovery queue 修复；`app_state.rs` 拥有 deferred bootstrap 排序与 Runtime task 注册；集成测试留在对应 owner 的测试模块以使用其 private lifecycle/test seams。文件已有体量不构成本切片拆模块或 crate 的收益证据；另立私有模块只移动代码，不会建立新 owner 或独立依赖边界。

## 验收与停止条件

- 注入一次批量读取错误后，Immediate dispatcher 的下一次读取成功，遗留 Pending session 自动 dispatch/完成一次；确认 dispatcher 没有因首次错误退出。
- 注入批量恢复错误后取消 token，重试在 backoff 期间退出，不启动第二次读取；当前已开始的读取不被 drop。
- actor 已存在但仍为 Pending 且不在 queue 时，再次恢复会将其入队并唤醒；非 Pending 状态不被恢复逻辑重新排队。
- 单 actor replay 失败继续跳过且健康 actor 可恢复；任意成功的批次调用（即使 `loaded == 0`）停止批次重试。
- Deferred App 启动在首次批量错误时注册 runtime-owned 重试并继续 Ready；初次恢复及后续重试在关机时都不 drop 已开始的批次调用，runtime 取消退避并 join task。
- 不改变 event/schema/UI 行为或模块边界；若无法确保取消/任务所有权，或队列重入造成重复执行，则暂停并修订决策，不扩展为通用恢复框架。

跨 Agent/Memory 持久恢复和 App task lifecycle 边界，完成时运行 `cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、crate dependency inventory、ADR index 与 `git diff --check`。

## 实施结果（2026-10-06）

- Immediate dispatcher 与 deferred App startup 共用 `SessionSupervisor` 的批次重试策略：250ms 指数退避、30s 上限、持续失败直到取消；`Ok(0)` 结束重试，单 actor replay/install 错误仍被隔离。
- Deferred 首次恢复和失败后的 retry 都注册为 `ApplicationRuntime` task。bootstrap await 首次 task 的结果以保持 Ready 顺序；bootstrap 外层取消不会 drop 正在运行的批次读取，runtime shutdown 会等待该 task 收尾，并取消 retry 的 backoff。
- `load_pending_sessions` 再次遇到已安装且仍为 Pending 的 actor 时会去重入队并唤醒 dispatcher；其他 actor 状态不由此恢复路径强制入队。返回值仍统计新安装数。
- Agent 回归覆盖 transient batch failure 后只 dispatch 一次、空批次成功停止重试、backoff 中取消、existing Pending actor requeue/wake，以及既有的单 actor replay 错误隔离。AppState 回归注入首次 deferred recovery 错误，确认 Runtime 注册 retry、bootstrap 达到 Ready 且 shutdown join 所有已注册 task。
- 门禁通过：`cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`scripts/check-crate-dependencies.ps1`（11 crates / 30 directed edges）、`scripts/check-adr-index.ps1`（505 ADR）、`git diff --check`。
- 不改 schema、IPC、配置或持久数据格式；无数据库迁移或重置要求。`session_events.rs`、ActionService、crate/API 与性能候选未出现新的准入证据，继续按 §5.5 触发条件复核。

## 回滚与重置

回滚 supervisor/App 的重试路径、Pending actor 幂等 requeue 和对应测试，并将本 ADR 从已完成退回 Deferred。没有 schema、持久数据、IPC 或配置变化，不需要迁移或重置；回滚会重新引入整批恢复失败后当前进程无自动恢复机会的生命周期缺口。
