# 1016：统一定时 ToolRun 执行权 claim 身份

## 状态

已采纳并实施（2026-10-12）。

## 背景

定时 ToolRun 执行前必须在 SQLite 中持久认领执行权，保证取消与副作用按先提交者获胜。确认路径以 confirmation request 的 `conf-*` ID 作为幂等身份；无需确认的 auto-approved 路径没有 request，Agent 按 Tools owner 的既有契约使用当前 `tool_run_id`。Memory 的 `ToolRunStore` 此前只接受 `conf-*`，导致生产组合根启用持久 store 时拒绝 auto-approved 路径，副作用不会执行。

## 决定

- 统一参数语义为 `claim_id`：确认路径使用其 `conf-* request_id`；无需确认路径使用与目标完全相同的 `tool_run_id`。
- Memory 持久边界只接受规范 `conf-*`，或等于目标 `tool_run_id` 的规范 `toolrun-*`。其它 `toolrun-*`、`claim-*` 和畸形值一律拒绝。
- 保留 SQLite claim key、事务顺序、取消竞态和终态清理逻辑；只对来源身份做更准确的验证。

## 影响

自动批准的定时操作恢复可通过 Memory 持久 store 原子认领；确认操作的行为不变。数据库 schema、已有 key/value 形状、IPC 与配置均不变，无需重置数据库。

## 替代方案

- 为 auto-approved 另造 `claim-*` ID：拒绝；该前缀属于进程内消息 inbox 领取令牌，且会多造一条持久身份生成路径。
- 一律接受任意规范 `toolrun-*`：拒绝；claim value 必须绑定目标 ToolRun，避免跨任务身份混用。
- 移除 Memory 校验：拒绝；持久 owner 仍须验证跨 crate 输入身份。

## 验证

- Memory store 回归：同一 ToolRun ID 可认领，另一个规范 ToolRun ID 被拒绝且不会留下 KV claim。
- Tools service 回归：启用 `ToolRunStore` 时同一 ToolRun ID 可认领，取消不能越过该 claim。
- 运行受影响 crate 的测试、编译、严格 Clippy 与 workspace 门禁。

## 回滚

恢复只接受 `conf-*` 的校验会让 auto-approved 定时操作再次在持久边界失败；不涉及数据回滚或数据库重置。
