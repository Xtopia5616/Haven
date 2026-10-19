# ADR 1327：恢复 Session grant 与 resolve event 分离提交

## 状态

已采纳（2026-10-19）；实施与门禁结果见下。

## 背景

[ADR 0519](0519-session-grant-resolve-append-failure-contract-test.md) 已固定 Session grant 先持久化、`interaction_resolved` 后追加的顺序，以及同一 live actor 内 append 失败后的重试行为；当时明确排除了 actor 在两次写入之间退出的情况。

该窗口内数据库可能已经包含 session grant 与 pending `interaction_requested`，但没有 `interaction_resolved`。Actor 退出后，内存 pending owner 和 expiry task 消失。`Paused` session 不会在启动时自动载入；未载入 actor 的 resolve 返回 `stale`。显式 `reopen_session` 会从事件流重建 pending request、从 grant 表恢复授权，并按原 `expires_at` 重新安排 expiry。这里需要说明 grant 是否补偿、同一请求能否重试，以及停机期间 deadline 如何处理。

## 决定

1. **不补偿已提交的 grant。** 用户选择 Session scope 后，grant 行就是已接受的持久授权决定；`interaction_resolved` 只确认当前 ToolCall continuation 已接受。Append 失败、actor 退出或请求之后过期，都不删除或回滚 grant。仅显式 revoke、reset、session 删除及既有 retention/cascade 会移除它。Grant 和当前 ToolCall continuation 保持两个独立提交边界。
2. **工具 continuation 以 resolved event 为准。** `interaction_resolved` 成功追加前，pending request 仍由 durable `interaction_requested` 表达，当前 ToolCall 不得继续执行。失败后，同 actor 或显式 reopen 恢复的 actor 可用相同 request ID 重试；grant upsert 保持幂等。成功 append 后，request 终态化且重复 resolve 返回 stale，不重放同一 continuation。
3. **未载入 actor 时不做隐式恢复。** Session owner 找不到 actor 时，resolve 返回 `stale`，不修改事件流或 grant。Startup 仍只自动载入 `Pending` session；paused session 的恢复由显式 `reopen_session` 触发。这样 stale 表示“当前 owner 不可路由或请求已终态”，不作为删除 durable request 的指令。
4. **期限保持原绝对 deadline。** Actor 未载入期间没有跨进程/跨 actor expiry timer，pending request 可以在 durable event log 中超过 `expires_at`。重开时按原 deadline 调度：未到期的请求仍可用原 ID 重试；已到期的请求立即追加 `Expired` 终态并唤醒 session 继续处理过期 observation，不执行原工具副作用。Expiry event append 失败时保留 pending request，并由 live actor 的 expiry task 按既有退避策略重试。
5. **UI 先恢复 owner，再呈现恢复的 pending confirmation。** 自动恢复历史 session 时，transcript 可以立即显示，但 pending confirmation 要等 `reopen_session` 成功后再 hydration，避免用户在 actor 未载入时点击得到 stale 并让 renderer 丢弃仍然 durable 的请求。其他历史打开路径继续先 reopen，再获取并投影 resume snapshot。

## 替代方案

- Append 失败后立即删除 grant：拒绝。删除本身可能失败；并且无法区分这次新写入与原本已存在或被后续明确确认的同一 grant，造成无原子补偿和错误撤权。
- actor 退出时立即终结/删除 pending request：拒绝。退出可能只是应用关闭；它会把可由 durable request 重试的用户决定变成不可恢复终态。
- 让 stale resolve 自动载入 actor：拒绝。resolve command 不拥有 Session restore 生命周期；隐式加载还会让一次过期 UI 点击改变 Session 工作集。由 `reopen_session` 作为明确恢复入口。
- 后台为未加载 paused session 保留 expiry worker：拒绝。timer 是进程内能力，跨进程保证需要新的持久任务 owner；当前契约选择在显式重开时按绝对 deadline 收敛，不承诺后台自动到期。

## 影响与验证

- 没有 schema、durable event、IPC shape 或 grant key 变化，无需重置用户数据。
- Session grant 仍由 Memory durable row 与 Tools live AuthorizationEngine 投影拥有；SessionActor 仍以 append-only interaction event 决定 continuation 是否可运行。
- 回归覆盖：resolved append 失败后 grant 保留；actor replacement 前 stale 返回不改 durable state；显式恢复后同一 ID 在 deadline 内可重试且只产生一条 resolved event；deadline 已过时重开立即写 `Expired`，grant 仍有效。
- UI startup 回归确认 auto-resume 在 `reopen_session` 完成前不 hydration pending confirmations，完成后才展示 replayed requests。
- 验收：定向 Agent 回归；`cargo fmt --all -- --check` 与 `cargo test --locked -p haven-agent`；UI `check`、`test:run` 与 production build；ADR index 与 `git diff --check`。

## 回滚

回滚只需恢复原 auto-resume interaction hydration 顺序、移除此 ADR 和新增回归测试。生产 durable 格式和用户数据未变；如果撤销此决定，路线图必须重新标记 actor-exit stale recovery 未定义，且不得声称 paused session 会在后台自动过期。

## 实施结果

- 新增 Agent 回归覆盖 actor 移除后的 stale resolve、显式恢复后同一 request ID 重试，以及已过期 request 在 reopen 后立即记为 `Expired` 并保留 durable grant。
- `cargo test --locked -p haven-agent`：666 项通过、1 项忽略；其中 session grant 定向回归 7/7 通过。`cargo clippy --locked -p haven-agent -- -D warnings`、`cargo fmt --all -- --check`、`git diff --check` 与 ADR index 检查通过。
- UI `check` 通过（0 errors / 0 warnings），startup 定向测试 9/9 通过，production build 通过。全量 UI 测试首轮为 1342/1343；唯一失败是未改动的 `MemoryView.test.ts` 用例达到 5 秒超时，同一用例单独重跑 1/1 通过。后续全量重跑持续输出 Vitest `TimeoutNaNWarning`，约 4 分钟未产生汇总，遂中断该重跑。
- `cargo test --workspace --locked --quiet` 已在共享 checkout 的另一条运行中执行；其最终结果尚未可读，本 ADR 不把它记为通过。
