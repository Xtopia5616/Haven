# ADR 0513：Windows 子进程先加入 Job Object 再恢复

## 状态

已采纳并实施（2026-10-05）。

## 背景

`ProcessContainment` 已在 `haven-platform` 封装 Windows kill-on-close Job Object，但旧 API 只有 `new()` 和 `attach(pid)`。MCP、Shell、Skill 与后台 Action 都先 spawn 再 attach；MCP 还在 spawn 后才创建 Job Object。新进程可在调用方获得 PID、分配 Job 之前执行用户代码并派生后代。Windows 不会把此前创建的后代追溯加入该 Job，因此取消或关闭 Job 不能保证回收整棵进程树。

Job Object 是生命周期清理边界，不是文件系统或网络沙箱；本决定不改变 opaque 子进程的授权策略。历史 ADR 0153 的“启动后加入 Job Object”时序由本 ADR 修正；ADR 0359 中 `attach(pid)` 和由调用方处理分配失败的 API 也由本 ADR 收口。

## 决定

1. Windows 上的受管子进程必须以 `CREATE_SUSPENDED` 创建。`haven-platform::ProcessContainment::prepare_command` 将该标志与调用方提供的完整 Windows creation flags 组合；调用方不得依赖 `CommandExt::creation_flags` 自动追加标志，因为它会替换整组 flags。
2. `ProcessContainment::attach_and_resume` 从 child 原始进程句柄取得 PID，并验证它与调用方 PID 一致，然后将该进程分配到 kill-on-close Job Object，再用 Toolhelp 找到该进程唯一的初始线程。它核对线程所属 PID 后只在 `ResumeThread` 返回旧挂起计数恰为 1 时成功。
3. 创建标志设置、Job 分配、线程快照/识别、线程打开、所属 PID 校验或恢复任一步失败时，`haven-platform` 终止该子进程并返回错误。调用方仍保留 `kill_on_drop`、错误清理、等待、取消和管道所有权；平台 crate 不接管工具业务或异步任务生命周期。
4. 所有当前 Windows 子进程入口都使用该顺序：MCP stdio（包括 Windows fallback 变体）、Shell、Skill 和后台 Action。Job 在 spawn 前创建。后台 Action 继续传 `CREATE_NO_WINDOW`；静默前台 Shell 传 `CREATE_NO_WINDOW`，可见前台 Shell 保留原窗口行为；其它入口保留原有创建 flags。
5. 非 Windows 保持现有 no-op containment 行为。此改动不变更授权、IPC、数据库、配置或持久数据，无需重置。

## 替代方案

- 继续先 spawn 后 attach：拒绝。Job 成员关系建立前运行的代码可能派生不会被追溯纳入 Job 的后代，破坏进程树回收保证。
- 在所有 adapter 各自实现线程枚举和恢复：拒绝。会把 Windows FFI 与失败策略复制到 MCP/Tools，并允许调用顺序再次漂移。
- 使用 `PROC_THREAD_ATTRIBUTE_JOB_LIST`：暂不采用。当前固定 Rust/Tokio 稳定 API 不支持向现有 Tokio command 注入自定义 startup attributes；手动 `CreateProcessW` 会要求重做 std 的命令行、环境变量和 piped stdio 构造，破坏面更大。
- 在 `haven-platform` 依赖 Tokio 并直接 spawn：拒绝。可以进一步包住调用顺序，但会让 OS 适配 crate 接管 async child API。当前由平台集中提供准备与原子化的 attach/resume 阶段，同时保持 Tokio 依赖留在现有消费者。

## 影响与验证

- `haven-platform` 仍是 Windows FFI 与 Job Object 句柄的唯一 owner，并新增 Toolhelp feature；不新增内部 crate 依赖边或 crate。
- Windows 单测启动挂起的 test helper，确认 attach 前它没有运行；恢复后 helper 立即创建长驻后代；关闭 Job 后父、子进程都必须退出。
- 失败关闭单测分别用 handle/PID 不匹配和注入线程发现错误触发失败，并确认挂起进程被终止且 helper 未执行；正向测试让 helper 在 attach 前有 1 秒执行窗口，并断言 `ResumeThread` 的旧挂起计数必须为 1。测试失败时会按 PID 文件清理可能的后代。
- 本切片在 Windows 环境执行适用 Rust workspace 门禁：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`、`pwsh -NoProfile -File scripts/check-crate-dependencies.ps1`、ADR 索引检查和 `git diff --check`。当前工作树的发布安装包/UI 验收仍按 ADR 0395 保持独立 Gate。

## 回滚

回滚实现提交、恢复各 adapter 的旧 spawn/attach 调用和旧 Shell creation flags，并从路线图移除此条完成项。该回滚会重新引入 Job 分配前子进程可执行的窗口；没有 schema、配置、持久数据或 IPC 需要恢复。
