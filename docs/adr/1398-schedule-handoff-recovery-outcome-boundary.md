# ADR 1398：Schedule handoff 以执行 claim 区分安全重试与结果未知

## 状态

已接受；恢复与拥塞故障测试及 Rust workspace 测试、严格 Clippy 已通过（2026-10-25）。真实 Schedule UI 仍按 ADR 1395 从本次发布 Gate 豁免。

## 背景

ScheduleRun 的 due claim 与副作用执行 claim 是两个边界。poller 在有界 fired queue 满时必须背压；进程在 channel 投递或消费者处理时退出后，不能仅凭 `running` 判断副作用是否开始。新增独立 job 状态会复制 Schedule 与 ToolRun 的生命周期，不符合现有职责边界。

## 决定

1. fired queue 保持有界，满时 poller 等待容量；消费者退出导致 send 失败时立即扫描并恢复仍为 running 的 occurrence。
2. 没有持久 execution claim 的 occurrence 尚未获准进入副作用执行：恢复为 waiting，沿用同一个 `schrun-*`，写清“安全重试”。周期 Schedule 复用该 occurrence，不创建重复 occurrence。
3. 有持久 execution claim 的 occurrence 可能已经开始副作用：恢复为 failed，写清“结果未知、自动重试已抑制”；绝不因进程重启自动重放。
4. 使用现有 status、execution claim 和 error_reason 表达边界；暂不增加第三套 job/status 表。`running` 只表示已从 due 集合领取并进入交接流程，execution claim 才表示已获准开始执行。
5. 恢复与 execution claim 使用 `BEGIN IMMEDIATE` 串行：恢复先提交时，迟到的 claim 会失败；claim 先提交时，恢复将其归类为结果未知。

## 验证

- Memory 测试覆盖未 claim occurrence 按原 ID 安全重试、已 claim occurrence 标记未知且不自动重放、周期 occurrence 身份稳定。
- Tools 测试覆盖 fired queue 满时背压、消费者退出时拒绝投递以及恢复未接收 occurrence。
- 真实 schedule UI 验收不在本 ADR 声称的范围内；既有 UI 豁免见 ADR 1395。

## 影响与回滚

没有 schema 或 IPC 变化。旧进程留下的未 claim running rows 会重新排队，已 claim rows 会终态失败并保留解释性原因。若回滚，实现必须至少保留“claim 后不重放”的安全边界；不应恢复一律失败或把 running 当成副作用已开始。
