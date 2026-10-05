# ADR 0514：显式结束会话的失败与重试契约

## 状态

已采纳，实施完成（2026-10-05）。

## 背景

`SessionSupervisor::end_session` 对 actorless session 使用 ActionService 的 checked cleanup；对驻留 Actor 则永久取消 actor lifetime、best-effort 清理所属 Action，并继续写入 `Completed`。scheduled action 的持久取消失败因此只在 actorless 路径返回错误；驻留路径可能成功结束会话，却留下仍为 `Waiting` 且可触发的定时任务。

Tauri `end_session` 只在 executor 成功后发 `session:completed`。UI 在 invoke 失败时保留当前 session 并提示“完成会话失败”。ActionService 的 checked cleanup 负责 scheduled durable row 的查询与取消；执行 claim 已获胜的 action 按 [ADR 0424](0424-interaction-lifecycle-ownership.md) 保持 first-wins。Background action 的 kill、终态持久化与有限重试策略由 [ADR 0507](0507-session-owned-action-cleanup.md) 保留。

## 决定

1. **结束成功的条件。** 显式 end 只有在 ActionService 完成 session-owned scheduled cleanup 后才可转为 `Completed`。scheduled durable 查询或取消出错时，命令返回错误，不发 `session:completed`，仍可执行的部分保持 durable 原状态；已成功取消的项保留为 `Cancelled`，重试只处理仍为 live 的项。
2. **失败时先保留可恢复状态。** 对尚未 `Completed` 的 session，end 先将状态持久化为 `Paused`。若该写入失败，不发 run cancel、不清理 actions，避免数据库仍显示活跃而 actor 已死亡。Action cleanup 失败或随后 `Completed` 写入失败时，session 保持 `Paused`，通过既有 `session:updated` event 投影 `waiting_reason=end_incomplete`，UI 保留选择并提示可重试。actorless 路径使用相同的 durable 状态顺序。已经 `Completed` 的 session 不回退状态。
3. **结束仍保持响应。** Paused 状态成功持久化后，只取消当前 run 的 child token，不取消 `actor_lifetime`，也不等待 provider/tool 退出。成功路径立即写 `Completed`；run-exit 负责延后清理。失败路径保留 resident actor，后续 run 可从 actor lifetime 建立新 child token。
4. **ActionService 继续拥有 Action 状态。** checked session cleanup 在同一 `spawn_gate` 下按 background、scheduled 顺序选择并清理两个 family，避免 admission 在 owner cleanup 已快照后才完成发布。run-scoped background admission 在等待 gate 时响应取消，并在拿到 gate 后复查 token。开始 durable 写入后，由持有 owned gate 的 worker 完成 SQLite 写入；若 caller 此时被取消，cleanup 等 worker 结束后还会查询该 session 的 durable background rows，并以既有 best-effort/有限重试策略收敛没有进入内存 board 的 live row。该 durable background 查询若仍失败，只记录错误并留待 restart recovery；它不属于 checked error，因此此路径可能暂时留下没有进程 owner 的 Running row。scheduled admission 等待 gate 时 caller future 被取消后不会恢复入场；若 durable worker 已开始，gate 会保留到写入结束，cleanup 再查询和取消 live row。checked error 只包含 scheduled durable 查询/取消错误；background 继续先发 kill、按既有 best-effort 规则写终态并在失败时启动有限重试。ActionService 与 Agent 不共享 SQL 写入口。
5. **执行 claim 不回滚。** scheduled action 的执行 claim 已赢时，cleanup 将其视为合法成功 no-op；该 action 继续执行，session end 可以完成。claim 未赢且 durable cancel 失败则 end 失败。不得为 end 清理而删除 claim 或改成 end-wins。
6. **确认 owner 与 end 串行。** SessionSupervisor 使用既有 `confirmation_resolution_gate` 将 owner resolve/expiry 与显式 end 的 action cleanup 串行。closing marker 期间新的用户 resolve 返回 stale；到期任务可重试，避免 end 失败后遗失 expiry。gate 在 Completed transition/cascade 前释放，closing marker 仍阻止 resolver 改变 owner 状态，避免级联 end 自锁。
7. **Run admission 必须明确。** Dispatcher run 使用其已持有的 slot；直接 run 必须在状态提升或任何 ReAct 副作用前拿到 `DirectRunLease`。direct admission 在生命周期 gate 内把 Pending/Paused 条件提升为 Running，确保实际执行与错误 CAS 状态一致；closing/已有 run/terminal status 导致准入失败时直接返回，不得把 `None` 当成 dispatcher 已 claim 而继续执行。
8. **取消优先于晚到错误。** run-exit 只通过 Actor 串行的 `Running → Error` 条件转移提交错误，不能在成功后再做无条件 Error 写入；直接 run 使用相同条件转移。若 end/interrupt 已先提交 Paused 或 Completed，错误不改状态也不发布第二个终态事件。自然错误仍在 run 持有 Running 时走既有错误路径。
9. **Rollback/Continue 与 end 串行。** 两者在等待 run 退出时不持有全局 lifecycle gate，以免阻塞 run-exit；在 durable transcript 改写前重新取得 gate、检查 closing marker，并持有到状态投影完成。end 已先取得 marker 时，rollback/continue 不得改写 transcript、交互或 session 状态。
10. **Continue 是用户明确覆盖 end 意图。** end 失败并显示 `end_incomplete` 时，用户仍可显式选择 Continue。遗留 action 保持 session owner 和常规生命周期；用户也可以再次执行 end 完成清理。UI 显示未完成原因，Continue 不会作为自动恢复策略触发。
11. 新增 `SessionWaitingReason::EndIncomplete` 只作为运行态/UI 投影，不写数据库或 session event。没有 schema、配置或 durable transcript 变更；新增枚举 wire 值向后兼容，无需重置数据。actorless session 在进程重启后可能根据遗留 action 投影为其他 waiting reason；显式 end 仍可安全重试，Continue 仍是明确的用户选择。
12. **终态清理使用单一 owner 与可恢复 lease。** status 转移、dispatcher run-exit 和 direct-run exit 都重读 Actor mailbox 当前状态，并在 lifecycle gate 下竞争 per-session terminal cleanup lease；清理和级联在全局 gate 外运行，结束后按 actor identity 与当前状态复核再移除。Continue、rollback、load、dispatch 和新的 closing owner 在 lease 内不能重开或删除 Actor。run-exit 不使用 FinishRun 的缓存状态，以免 Continue/rollback 在 run bit 清零后先提交 Pending/Paused 时，旧 run-exit 再删除 Actor或丢队列。idle Error 清理保留 Continue 所需 Actor，run-exit Error 清理仍移出工作集；lease 重试携带 `remove_error_actor` 与级联策略，不混淆这两种语义。lease 被取消/异常释放时，session id 进入去重队列，由现有 dispatcher 主循环重读状态并重试，不新增常驻服务。完成清理后丢弃重试意图。
13. **End closing owner 与阶段显式化。** marker 区分 `EndPreparing`、`EndCommitted` 和 `Destructive`。run-exit 在 EndPreparing 或删除/移除期间不得接管；持久化 Completed 成功后立即原子提升为 EndCommitted，后续 run-exit 才可接手，并沿用 End 指定的 cascade 策略。该策略在 close marker 释放后仍保留到终态清理完成。重复 close 先检查再插入，不能覆盖原 owner。取消的 EndCommitted/Destructive marker 会唤醒同一 dispatcher 的重试仲裁；actor 仍运行时保留 End cascade 意图，直到实际 run-exit。
14. **direct-run 收尾有单一生命周期线性化点。** `run_session_from_id` 在 ReAct future 返回后 await direct-run exit reconciliation，因此调用返回后 Continue 不会撞上仍持有 lease 的善后窗口。run bit 的释放和 `FinishRun` 在 lifecycle gate 内串行执行；每个 direct run 另持有按 session 与 lease id 登记的 admission reservation，直到 reconciliation 完成才释放。direct admission 和 dispatcher claim 都尊重该 reservation，避免 run bit 清零后的同 session 重入。Drop 把完整 lease 交给异步兜底任务，先取消 run child token，再向同一 Actor mailbox 放置屏障并等屏障前已提交的 Actor-owned ReAct future 退出，完成 reconciliation 后才归还并发 permit。Paused direct run 在副作用前持久化 `Running`；统一状态表允许 `Paused → Running`。
15. **状态 watch 保存最后值。** Actor 状态发送使用 `watch::Sender::send_replace`，即使当时没有订阅者，新订阅者也读取当前状态；终态仲裁仍以 Actor mailbox 快照为权威值，不把 UI watch 缓存当作可变状态来源。memory-only status API 拒绝 terminal 状态，避免未持久化 Completed 移除仍可恢复的 Actor。
16. **direct-run admission 与清理绑定 Actor identity。** admission 在第一个可取消的等待之前安装 waiter RAII；permit 到手后，在 lifecycle gate 内确认 registry 仍指向同一 Actor，并先创建完整 lease 与同 session admission reservation，再提交 Running 和 actor run bit。若 admission future 在这些 await 之间被取消，lease Drop 负责取消该 Actor 的 run token、等 mailbox 屏障前已提交的 ReAct loop 结束并做状态/终态 reconciliation；等待 admission 本身被取消时 RAII 注销 waiter。finish/Drop 使用 lease 中的 actor handle，并把同一 expected actor 传给 run-exit reconciliation；reservation 以 lease id 条件移除，避免旧 Drop 清除后续 reservation。旧调用者延迟退出时，即使 session id 已被移除并重新加载，也不能清除新 Actor 的 running 位或清理它。

## 替代方案

- 仅把 resident Actor 的 best-effort 调用改成 checked：拒绝。若仍先永久取消 actor lifetime，action cleanup 失败会留下不能继续的 actor；若直接转 Paused，也没有保护 confirmation resolution、direct-run admission 与 late run error。
- 等待 provider/tool 退出后再返回：拒绝。已有控制命令要求 end 对卡住的 run 保持响应；当前 run token 可以单独取消。
- cleanup 失败时回滚已经成功取消的 action：拒绝。ActionService 对每个 action 独立仲裁，已提交终态不可逆；重试以剩余 live rows 收敛。
- 清除已获执行 claim 的 action 或让 end 强制获胜：拒绝。破坏 ADR 0424 的跨实例 first-wins。
- 把 background action persistence failure 一并变成 checked error：暂不改变。后台进程已先收到 kill，存储失败继续走现有有限重试；该策略已在 ADR 0507 中明确，与 scheduled timer 仍可被触发的风险不同。
- 让 SessionStore 直接更新 Action 表：拒绝。ActionService 仍独占 action state、claim CAS、outbox 与终态事件。
- 禁止 EndIncomplete 状态下 Continue：拒绝。显式 Continue 是用户选择恢复 session 的入口；失败提示与 waiting reason 会明确指出 end 未完成，遗留 action 继续遵循 owner lifecycle，用户可再次 end。

## 影响与验证

- actorless、idle Actor 与 running Actor 的 end 失败都保留 `Paused`、当前 session 与可重试入口；scheduled action 部分成功保留，retry 幂等。
- 正常 end 仍不会等待 stuck run；session completion 事件只在 executor 成功后发送。
- confirmation resolve 与 expire 不会跨过 end closing marker 修改 request/claim 状态；direct run 被拒绝时不再执行未登记 ReAct run；rollback/continue 在 durable 改写前复核 closing marker。
- 取消后的错误不会覆盖已接受的 Paused/Completed lifecycle 状态；未经 end/interrupt 接受的自然错误仍发布一次 `SessionError`。Direct run 从 Paused 开始时先持久化 Running。End 与 run-exit 交接时 terminal cleanup 恰好由一个 owner 执行；Continue 先提交 Pending 时 run-exit 会保留 actor 并重入队；清理 claim 先获胜时恢复 admission 被拒绝。EndPreparing 与 Destructive close 会阻止 run-exit 抢先清理；EndCommitted 的 cascade 策略跨 run-exit 保留。cleanup owner 取消后由 dispatcher retry worker 回收，普通 idle Error retry 保留 Continue actor。late watch subscriber 读到最新状态；重复 End 可重试已提交 Completed 后未完成的清理。
- 验收覆盖 actorless、resident/idle、running/stuck run、status write failure、单项失败与多项部分失败、retry、claim-wins/cancel-wins、confirmation resolve/expiry race、background/scheduled admission gate 取消、无内存 board 的 durable background row 回收、rollback/end 与 continue/end admission、run-exit error CAS、direct-run admission 取消后的 waiter 注销、actor-owned ReAct loop 取消与 permit 保留、ReAct mailbox 尚未处理时的屏障等待、finish/reconciliation 取消时同 session admission reservation 保持到清理结束、旧 Actor 延迟清理与新 Actor 隔离，以及 UI paused/error event projection。
- 无 schema/reset，需同步 Rust `SessionWaitingReason`、生成的 TypeScript command contract、UI label/reducer 以及 IPC contract 检查。IPC generator 忽略不带 Serde 的平台内部 type alias；Windows/non-Windows 同名 cfg alias 不属于 DTO 重复。
- 门禁结果（2026-10-05）：`cargo fmt --all -- --check` 通过；`cargo test --workspace --locked` 通过（工具链 workspace 全部测试通过，性能 profile 测试按约定忽略）；`cargo clippy --workspace --locked -- -D warnings` 通过；UI `check` 通过（0 errors / 0 warnings）、`test:run` 通过（122 files、984 tests）、production `build` 通过；IPC command contract 通过（79 handlers）、IPC event contract 通过（40 channels）；crate dependency inventory 通过（11 crates、29 条单向边）；ADR index 通过（497 条唯一编号记录、链接可解析）；最终 `git diff --check` 通过。Windows 安装包发布验收仍是独立 Open Gate，不属于本 ADR 的本地门禁结论。

## 回滚

回滚 Agent end status/action 协调、confirmation closing 检查、direct-run admission 修正、ActionService 双 family gate 与 UI `EndIncomplete` 投影，恢复原 checked actorless 与 best-effort resident 行为。没有数据库迁移或重置；已有 paused session 可按原状态继续或重新结束。回滚会重新引入 resident end 静默遗留 scheduled action 的风险，因此需同时将路线图此项恢复为 Deferred。
