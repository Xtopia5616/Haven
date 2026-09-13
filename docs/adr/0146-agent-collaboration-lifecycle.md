# ADR 0146：Agent 协作生命周期与可恢复消息契约

## 状态

已接受（2026-09-14）。本 ADR 补充并细化 Agent 间协作的消息与生命周期边界。

## 背景

`agent.*` 原有能力可以发现 Agent、发送消息和创建子 Agent，但消息 claim 与 ack
绑定在同一次调用中，调用方在处理消息前崩溃时容易丢失可恢复入口；同时缺少对子 Agent
的直接查询、历史读取、等待和停止能力。`agent.profile` 的读路径也不应要求无关的
更新字段，`agent.list` 需要能够按能力和父子关系收窄结果。

## 决定

Haven 将 Agent 协作收敛为以下契约：

1. `agent.inbox` 默认只 claim，不 ack；处理完成后调用方显式使用
   `agent.ack(message_ids)` 选择性确认。一次性兼容调用可以显式传 `ack: true`，但
   未确认的消息仍保持 durable processing 状态，可由恢复流程继续处理。
2. `agent.children` 返回当前 session 的直接子 Agent；`agent.history` 是只读的、带
   上限的历史读取入口，目标限定为当前 session 或其后代。
3. `agent.list` 支持 `role`、`capability`、`parent`、`status` 和 `limit` 过滤；空参数
   的 `agent.profile` 读取当前注册信息，带更新字段时才执行 profile 更新。
4. `agent.status`、`agent.wait`、`agent.stop` 通过 `AgentController` 回调连接 Agent
   运行时。status 允许查看自身，wait/stop 只允许控制后代；wait 使用有界超时和状态
   watcher，stop 复用正常会话终止路径，并保持高风险确认。
5. peer Agent 正常结束时保留 inbox registry 的父子元数据，只标记为离线；这样历史、
   状态和未确认消息仍可定位。普通 unregister 路径仍用于没有父 Agent 的会话。

`AgentTool` 仍属于 `haven-tools`，不直接依赖 `haven-agent`；宿主在组合层注入
`AgentController`，以避免工具层与 ReAct 执行器形成循环依赖。所有新 operation 都进入
统一的 operation view、schema、prompt、风险矩阵和测试目录。

## 未采用的方案

- **让 `haven-tools` 直接调用 `haven-agent`**：会形成反向依赖，并把工具契约和执行器
  生命周期耦合在一起。
- **用 heartbeat 推断状态**：不能可靠区分暂停、完成、崩溃和停止请求，也无法提供有界的
  wait 语义。
- **inbox 每次 claim 都整体 ack**：会让批处理中尚未成功处理的消息不可恢复。
- **允许任意 Agent 读取或控制其它 session**：扩大了跨会话数据和控制面，超出协作所需的
  最小信任范围。

## 影响、验证与回滚

本变更不修改 SQLite schema、实体 ID 格式或 provider wire 协议；它扩展 `agent.*` 的
operation schema 和运行时回调，并改变 inbox 默认 ack 语义。旧调用方应显式传 `ack: true`
保持一次性处理行为；新调用方使用 claim → process → selective ack。

验证至少包括：

- `cargo fmt --all -- --check`
- `cargo test --locked -p haven-tools`
- `cargo test --locked -p haven-agent`
- `cargo check --locked -p haven-app-binary`
- `cargo clippy --workspace --locked -- -D warnings`

回滚代码即可恢复旧 operation 实现。回滚期间已经处于 processing 的消息不应直接删除，
应由旧版 retry/恢复路径重新接管；无需数据库重置。
