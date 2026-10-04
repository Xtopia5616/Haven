# ADR 0423：权限确认等待期限与完成确认

- 状态：Accepted
- 日期：2026-10-02
- 关联：ADR 0046、0109、0188、0402；`interaction:requested` renderer projection

## 背景

确认弹窗在 IPC 调用完成前就把请求标记为已解决。后端若因会话授权持久化、事件写入或票据校验失败而拒绝提交，renderer 已经移除了唯一可操作的卡片，会话仍可能停在 `Paused`。此外，确认票据有绝对有效期，但交互 DTO 没有公开该期限；renderer 的倒计时会在恢复或排队后重新开始。超时与用户主动拒绝也共用同一结果，Agent 会把超时误认为用户拒绝。

## 决定

1. 确认交互携带其授权票据的 `expires_at`。renderer 倒计时取显示期间的 120 秒上限与票据绝对到期时间中的较早者；恢复和排队不会延长票据寿命。
2. `resolve_confirmation` 接收 `timed_out`。超时始终按拒绝处理，不写入会话或永久授权；后端把交互置为 `expired`，并让 ReAct 确认批次继续，以明确的“超时、未执行、不要自动重试”观察结果结束该调用。
3. 普通和定时确认在后端都有票据到期看门狗。前端超时提交与后端看门狗通过同一确认锁竞争，只有一个终态决策会生效。定时确认另保留 30 分钟上限，实际期限取票据期限与该上限中较早者。
4. 会话级允许在票据验证失败时不写授权；后端将请求消费为过期拒绝并恢复会话，再向 renderer 返回可识别的失效错误。
5. actor 恢复时保留已解决的确认请求，直到工具结果提交并追加 interaction-clear 事件。进程若在用户决定后、结果提交前退出，恢复流程仍能使用原决定完成同一工具批次。
6. renderer 只在后端确认提交成功后移除弹窗。可重试的后端错误会保留当前请求；已失效或已消费的请求由后端状态事件或 stale 响应清除。UI 直调请求已由后端消费后，即使执行返回错误，也不会重新开放同一请求以避免重复副作用。

## 安全与替代方案

- 过期一律 fail closed，不刷新或延长原票据，也不把用户曾选择的会话/永久范围应用到过期请求。
- 保持票据 5 分钟有效期并把超时伪装成普通拒绝会混淆执行结果；让 renderer 在 IPC 完成前乐观清除会使后端错误重新变成不可恢复的挂起会话。
- 交互事件只投影安全字段；原始工具输入和授权票据仍留在后端。

## 兼容与重置

`resolve_confirmation` 增加 `timed_out` 请求字段，Rust handler、生成的 TypeScript 命令 map 和 IPC 文档同步更新。应用与 renderer 随同一版本发布；数据库 schema 和 transcript 重置边界不变，无需重置用户数据。

## Owner 路由后的期限契约

本节由 ADR 0424 取代本 ADR 第 1–3 条中的 renderer 120 秒决定、`timed_out` 输入和 ScheduledAction
30 分钟上限。ADR 0424 的 owner deadline 契约已实施：pending permission confirm 登记时必须有有效、未来的
`expires_at`，该绝对期限投影到 renderer；renderer 只显示倒计时，不再提交 `timed_out` 或自行创建
120 秒期限。Session、ScheduledAction 和 AppCommand 各由自己的 owner timer 过期；resolve 与 expire
在 owner 仲裁中比较同一个期限。Session 恢复时发现缺失或无效期限的历史 pending confirm 会立即过期；
若 durable expiry event 写入失败，owner 保留请求并退避重试。无效新期限不会进入 pending registry。

默认期限来自授权 receipt；目前 ScheduledAction 不再另设 30 分钟 fallback 或期限推导。Rust command、
生成 TypeScript contract、运行时 mapper 与 IPC 文档已删除 `timed_out`。数据库 schema 和 transcript
重置范围不变。

## 验证

2026-10-05 期限收口实现通过 `cargo test --workspace --locked`、
`cargo clippy --workspace --locked -- -D warnings`、`cargo fmt --all -- --check`、
UI `corepack pnpm run check`、`corepack pnpm run test:run`（120 个文件、962 个测试）和
`corepack pnpm run build`。`scripts/check-ipc-contracts.ps1` 核对 79 个 handler，
`scripts/check-ipc-events.ps1` 核对 40 个 event channel；手工性能 profile 按项目约定保持 ignored。
