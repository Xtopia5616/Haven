# ADR 0507：会话终止与删除清理所属 Action

## 状态

已采纳；实现与门禁通过（2026-10-05）。

## 背景

`SessionSupervisor::end_session_inner` 与 `quiesce_session` 只在 `SessionActor` 驻留时调用 ActionService 清理。actorless session 的显式 end、delete 与 retention 删除会跳过 action owner。`clear_sessions_and_delete` 也只 quiesce actor registry 中的 session。`actions.session_id` 没有指向 `sessions` 的外键，因此 session 删除不会自动处理所属 action。

ActionService 目前按本实例内存 board 选择所属 action。scheduled action 在启动恢复前仍只有数据库 `waiting` 行；若此时删除 session，该行会在后续启动再次恢复。不同 ActionService 实例各自持有 `spawn_gate`，另一个实例还可能已把任务推进为 durable `running`，导致仅查询 waiting 行的清理漏过它。`restore_pending` 也必须与本实例的取消共用 `spawn_gate`，避免在清理完成后 hydrate 已读出的旧 waiting 行。

scheduled execution claim 是 ADR 0424 定义的 first-wins 边界：ActionService 的进程内锁序为 `spawn_gate → terminal_transition`，数据库取消语句只在没有 `scheduled_execution_claim.<action_id>` 时将 waiting/running action 改为 cancelled；claim 在 SQLite writer transaction 中认领 running action。此决定必须保持。

## 决定

1. 显式 end、单 session delete/retention 与清空并删除所有 session 都调用 SessionSupervisor 的 session-owned action lifecycle；是否存在 actor 不改变调用。end 和 delete 都先设置 session-closing marker，阻止并发 load/resume；delete 路径 quiesce actor run 后再做最终 owner cleanup，最后删除 session。普通 shutdown 继续只取消 background action，不取消 waiting scheduled action。
2. ActionService 是所属 action 的唯一状态 owner。session cleanup 在 `spawn_gate` 下先沿既有 board 路径取消已 hydrate 的 scheduled action，再枚举 durable waiting/running scheduled rows，并对不在本地 board 的所属行使用现有 claim-protected CAS。这样即使另一个实例已经持久化 timer fire，cleanup 也会参与数据库状态仲裁；CAS 与执行 claim 由 SQLite 写入顺序决胜。不会由 Agent 或 SessionStore 直接写 actions 表。
3. `restore_pending` 在同一 `spawn_gate` 下读取 pending rows 并 hydrate board。过期任务的立即 fire 延至释放 gate 后，避免重入锁。这样 restore 与 cleanup 的 list/hydrate 和 owner cancel 互斥：restore 先完成则 cleanup 看见 board 项；cleanup 先提交则 restore 查询不再返回已取消行。
4. 定时任务持久化 admission 在 DB 写入期间继续持有 owner gate，即使调用方 future 被取消也一样；取消完成后，清理可观察并处理已经提交但尚未发布到内存 board 的 row。取消/claim 仍由 ADR 0424 的进程内锁与 SQLite CAS 先后决胜。
5. lifecycle cleanup 对持久读取或写入错误采取 fail-closed：单 session 删除及批量删除不得在所属 live scheduled row 未能仲裁时删除其 session。CAS 返回 false 视为另一终态或执行 claim 已获胜，不覆盖 claim，也不阻止 session lifecycle 继续；已获 claim 的执行仍 first-wins。后台 action 保持现有取消策略。
6. 持久化 schema、IPC、action payload 和 session event 均不变；不新增通用 Job 抽象、第二 action registry 或跨层 SQL owner。

## 替代方案

- 只把 actorless 分支中的调用补上：拒绝。ActionService 只扫描内存 board，无法清理未恢复的 durable waiting row。
- 让 SessionStore/Agent 直接 UPDATE actions：拒绝。会复制 ActionService 的终态、claim 与事件发布规则，破坏单一 action owner。
- 在清理后再靠 session_id 内容或后续恢复过滤 orphan：拒绝。它不会取消可独立执行的 scheduled tool，也会把终态处理推迟到重启。
- 改动 ADR 0424 的 claim 语义或直接清除 claim：拒绝。已接受的执行权仍 first-wins。
- 让普通 shutdown 清理 scheduled work：拒绝。shutdown 必须保留 scheduled waiting rows，以便下次恢复。

## 影响与验证

- actorless end/delete、retention 和全量删除都会经过同一个 ActionService owner；未恢复的 waiting rows 以及其他服务实例已启动的 running rows 都会进入 owner cleanup。
- 同一实例的恢复与取消在 Service gate 内线性化；跨实例的 start、cancel 与 execution claim 由既有 SQLite CAS 仲裁。
- 若持久取消失败，删除返回错误并保留 session row，调用方可在存储恢复后重试。execution claim 已获胜的 action 按 ADR 0424 继续，不被强制取消。
- 无 schema、配置、IPC 或 durable event 变化，无需重置用户数据。
- 验收覆盖 actorless end/delete/全量删除、尚未恢复的 waiting row、另一 ActionService 已启动的 running row、restore 与 cleanup 互斥、claim-wins、持久错误 fail-closed，以及 shutdown 保留 scheduled work。门禁按跨 Agent/Tools/Memory 的 workspace 要求执行。
- 验证：`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo fmt --all -- --check`、`git diff --check` 与 ADR 索引检查通过。workspace 测试首轮有一项 Agent run-exit 事件边界用例间歇失败；该用例单独复跑和 workspace 全量重跑均通过。

## 回滚

可回退 Agent 对 actorless session 的清理调用与 ActionService 的 restore/owner-cleanup 串行化，恢复原行为；不涉及持久 schema 或数据迁移。若回滚，应同步把 ADR 0507 标为撤销，并在路线图重新记录未解决的 orphan 风险。
