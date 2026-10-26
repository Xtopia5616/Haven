# ADR 1432：终态清理重试的合并唤醒与独立工作窗口

## 状态

已实施 — 2026-10-27。

## 背景与证据

R04 源码复核确认，`TerminalCleanupRetryQueue` 虽按 session 合并待办，仍向无界通道写入 session ID；dispatcher 的两个主循环分支直接 await 清理重试。清理会访问 partial、inbox、工具 overlay、后代会话和授权状态，慢适配器会阻止主循环准入其他会话。已有 shutdown 等待 run wrappers、最终尝试重试队列和 30 秒总期限的保证需要保留。

## 决定与不变量

1. HashMap 继续作为进程内待办和策略合并权威，不因容量或通知失败丢弃请求；同锁 FIFO 索引只为新身份入队一次，取消或超时项重排到队尾，防止失败项饿死其他会话。唤醒改为只表达“存在待办”的合并通知，不再复制独立的 ID 通知队列。
2. 重试由 dispatcher 生命周期内的单个受管 worker 串行执行，不在 admission select 中 await。每批最多处理 32 项后让出执行；初次 End/run-exit 清理路径不属于本次迁移。
3. 从 map 取出待办时即持有私有 retry lease，正常完成才解除；取消或超时将完整的 cascade/remove-error 策略合并回队列，覆盖取得 `TerminalCleanupGuard` 前的取消空窗。
4. 单次重试以 5 秒为限；超时后保留待办并退避 1 秒。既有 actor identity、生命周期 gate、End 与 Delete 区分及 cleanup guard 保持不变。
5. shutdown 停止准入，等待 run wrappers 的晚到重试，再中断 worker 的普通尝试（lease 重排队），按此时待办数逐项进行一轮最终尝试并 join；不预先 drain 到局部 Vec。失败项排到队尾，不在最终轮次重复尝试；整体仍受现有 30 秒 dispatcher join 期限约束。到期终止其拥有的异步任务；已开始的 `spawn_blocking` 操作不承诺被 future drop 终止。

## 容量与恢复边界

本切片只证明唤醒存储固定、异步重试窗口并发为 1、batch 有限，以及 admission 主循环不再直接等待重试。初次 run-exit 清理仍在原 wrapper 中持有该 run 的 admission permit，正常 End/Delete 也不迁移；不声称所有慢清理都与 run 容量脱钩。待办 map 按不同 session 数增长，没有新增硬上限。retry 策略和 cascade override 仍在内存，startup 仍只恢复 pending session；不新增跨重启清理保证。若继续要求整个 backlog 有界或重启补做，须另行定义持久标记/可恢复扫描，并保留 `cascade=false` 等原始策略，不能截断 map 冒充容量治理。单次 timeout 不保证终止已经启动的 blocking/OS 副作用，异步窗口并发上限不等于物理副作用并发上限。

## 替代方案

- 仅替换无界通道：不能消除慢重试对新 run admission 的阻塞。
- 为每个请求裸 spawn：会失去并发、取消和 shutdown ownership 上限。
- 给 map 加上限并丢弃新请求：Drop 产生的必须清理工作会丢失。
- 本轮顺带引入持久 cleanup job：扩大 schema、恢复与 End/Delete 契约，独立于已确认的调度问题。

## 影响、重置与回滚

只改变 Agent 内部清理重试的调度与任务所有权；无公开跨 crate API、IPC、数据库、配置或事件格式变化，无需重置。重复清理仍复用既有幂等/身份校验。回滚恢复 dispatcher 重试分支和原队列，不改变持久 session 状态。

队列、retry lease 与 worker 放在私有 `session/terminal_cleanup.rs`，dispatcher 只持有和等待任务，`status.rs` 仍拥有清理领域行为。超过职责复核阈值的 `session/mod.rs` 不再增加调度实现；新增行为夹具单独放在私有测试模块，不扩大生产 facade。

## 验证

- 回归先行：旧实现下 `blocked_terminal_cleanup_does_not_block_run_admission` 在阻塞 overlay 清理时未能准入新 handler，以 `cleanup must not occupy the admission loop` 失败；随后才接入 worker。
- 主机已有开发运行占用默认 target 构建锁，本轮使用 `CARGO_TARGET_DIR=target/r04-verification` 与 `CARGO_INCREMENTAL=0`，不终止既有开发进程。
- 定向 `cargo test --locked -p haven-agent --lib terminal_cleanup -- --nocapture`：最初 11 项通过；新增真实 30 秒退出用例后单跑该用例通过（30.17 秒），验证 8 项策略完整保留、异步 worker 和清理 lease 均无残留。
- `cargo check --locked -p haven-agent`：通过。
- `cargo clippy --locked -p haven-agent --all-targets --no-deps -- -D warnings`：通过；并修正 Agent 既有测试辅助代码的模块顺序、冗余 struct update 和可合并条件 lint，不改变生产行为。
- 未加 `--no-deps` 的 all-targets Clippy 被未修改的 Tools 依赖阻断：`builtin/clipboard.rs` 的 `len_without_is_empty`，以及 `mcp_server_config.rs` 两处 `needless_return`。这些不是本切片引入的告警；本轮严格门禁限定于受影响的 Agent 全部 targets，不声称 workspace 严格 Clippy 通过。
- `cargo test --locked -p haven-agent -- --test-threads=4`：697 passed、0 failed；1 项单元用例和 2 项手动性能 profile 按原配置 ignored，doc tests 通过。新 30 秒期限回归也在全量运行中通过。
- `cargo fmt --all -- --check`、ADR index（1329 条）与 diff 检查通过。本切片不改跨 crate 公共契约或 IPC，未运行 workspace/前端新一轮门禁；前一 UI 阶段的验收见 ADR 1431。实际 Windows 发布 Gate 仍保持开放。
