# ADR 0421：LLM 手动重试与熔断状态探测

## 背景

Router 熔断器打开时，普通请求和顶部连接探测都会在本机快速失败。该拒绝此前伪装成
`ServerError`，连接探测因此将未发生的 provider 请求记为服务器故障并写 WARN；用户对
错误会话点击 Continue 也会被同一冷却窗口挡住。

## 决定

- 将本机熔断拒绝表示为 `LlmError::CircuitOpen`，连接状态理由使用 `circuit_open`。
  顶部探测在此结果下不访问 provider，只写 debug 记录；真实网络/auth/server 探测失败
  仍写 WARN。
- 用户对错误或暂停会话执行 Continue 时，重置所选 Chat 模型的连续失败门槛，使排队的
  新 run 可以立即尝试。保留失败/调用累计计数及 rate-limit cooldown；新的连续 provider
  失败仍会重新打开熔断器。
- 不改变 3 次失败阈值、30 秒自动冷却、HalfOpen 探测、provider 协议、数据库或配置格式。
  `circuit_open` 是现有连接报告的新增失败分类，前端明确说明 Continue 可以立即重试。

## 替代方案

- 让用户等待 30 秒：会使明确的 Continue 操作在请求发出前失败，拒绝。
- 让所有 health check 无视熔断：周期性 UI 探测会绕过统一的冷却窗口，拒绝。
- 将 CircuitOpen 继续表示成 server error：会把本机拒绝误报为 provider 故障，拒绝。

## 影响与回滚

只有进程内 endpoint health 与连接报告分类改变，不需要用户重置数据。回滚代码即可恢复
原有行为；新增的 `circuit_open` 前端分类属于当前版本 IPC 枚举，无持久兼容状态。

## 验证

- Router 回归确认 open-circuit status probe 不调用 provider、返回 `circuit_open`，手动重置后
  下一次探测可以到达 provider；Agent Continue 集成回归确认已打开的聊天断路器会在用户
  重试时清除；Endpoint health 回归确认重置连续失败状态并保留历史计数。
- UI 回归确认 `circuit_open` 文案指导用户通过 Continue 立即重试。
- 定向复跑：`cargo test --locked -p haven-agent continue_session_resumes_errored_session` 通过。
- 通过：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、
  `cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`、
  `corepack pnpm --dir ui run check`、`corepack pnpm --dir ui run test:run`（898 项）、
  `corepack pnpm --dir ui run build` 与 `pwsh -NoProfile -File scripts/check-ipc-contracts.ps1`
  （75 个 command contract）。
