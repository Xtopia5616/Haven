# ADR 0520：SessionActor interaction replay 失败时 fail closed

## 状态

已完成（2026-10-06）。

## 背景

ADR 0502 将 `session_events` 定为 SessionActor interaction lifecycle 的唯一恢复来源。ADR 0294 在把交互事件读写迁移到 `SessionStore` 时，另行决定 `install_actor` 遇到读取或 replay 错误后 warning 并以空 interaction 列表继续注册 actor。该 fallback 会把 durable interaction state 的未知/不可读状态表现成“没有 pending interaction”；actor 注册后，`ensure_session_loaded` 又会直接复用它，不再触发 replay。

同一安装路径还会恢复该 session 的持久授权 grant。当前顺序是在 interaction replay 前应用 grant，因此坏 event stream 会留下空交互 actor并激活 session grant。pending sessions 的批量启动恢复则在任一 actor 安装错误时立即退出，后续健康 session 不会在本轮恢复。

这不是 crate 或模块体量问题，而是事件恢复错误被降级成合法空状态，并扩大到同批其他 session 的恢复可用性。步骤 0 复核确认，ADR 0294 的 fallback 早于 ADR 0502 的 event-sourced recovery contract；本切片只调整恢复失败语义与隔离，不重划 SessionStore/SessionActor owner。

## 决定

1. `SessionSupervisor::install_actor` 先从 `SessionStore` 读取并 replay active interaction events。读取或 reducer 错误必须向调用者传播；在 replay 成功前不得恢复该安装路径上的 session grants、spawn actor、写入 actor registry 或安排 confirmation expiry。
2. replay 成功后按现有顺序恢复 durable grants，再注册带有 replay 结果的 actor。`ensure_session_loaded` 对未注册 session 仍可重新进入安装路径并重试；不缓存失败结果。
3. `load_pending_sessions` 对每个 pending session 独立尝试安装。单个 session 安装失败时，以 `session_id` 和错误记录 warning，跳过该 session 并继续处理其余记录；pending session 列举本身失败仍返回错误。返回值只计入成功安装的 session，dispatcher 在至少加载一个 session 后照常唤醒。
4. 仅替代 ADR 0294 决策 2 中“replay 失败后安装空 actor”的 fallback。ADR 0294 的 SessionStore 异步读写边界、payload owner、append 语义继续有效；ADR 0502 的 event replay 权威不变。

## 非目标与影响

- 不修改 event 类型/payload、schema、IPC、transcript projection、其他确认 owner 或授权规则；不迁移/重置用户数据库。
- 不删除或修复坏 durable event。坏 session 保持 actorless 并产生可观测日志；本切片不启动定时/退避自动重试，只有后续显式加载或另一次 pending recovery 调用才会重新尝试。
- 不改全局安全策略更新时对 durable session grants 的既有重应用契约；本 ADR 约束 actor 安装的先后顺序。
- 单个坏 pending session 不再阻断同一启动批次中的健康 session；如果全部 session 都失败，函数返回 `Ok(0)`，各失败由逐会话 warning 留痕。

## 验收与停止条件

回归测试在内存 SQLite 中创建两个 pending sessions：一个含有合法 JSON 但无法解码为 `InteractionRequest` 的 `interaction_requested` event 和持久高风险 operation grant，另一个为健康 session。断言：

- 批次恢复跳过坏 session 并成功安装健康 session；返回计数只包含健康 session。
- 坏 session 没有 actor，持久 grant 没有经失败的 actor 安装路径进入 live `AuthorizationEngine`；随后 `ensure_session_loaded` replay 仍报错且不缓存 actor。
- pending event 的 replay 错误被保留为调用错误，而不是转换成空交互状态。

运行 `cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`scripts/check-crate-dependencies.ps1`、`scripts/check-adr-index.ps1` 和 `git diff --check`。该切片跨共享持久恢复与安全状态，使用 workspace 门禁。

若 reducer 对 malformed interaction event 有意定义了跳过语义，或发现损坏 session 仍能通过其他入口执行工作，应暂停实现并先修订恢复/授权契约，不得把坏事件静默过滤掉。

## 回滚

移除 ADR 0520 的生产变更和回归测试，恢复 ADR 0294 当前 fallback，并把路线图 Active 状态回退为复核所得状态。无 schema、IPC 或用户数据变化，无需数据迁移/重置。回滚会重新允许 replay 错误被伪装成空 interaction state。

## 实施结果

- `install_actor` 先 replay interaction events，成功后才恢复 grants、注册 actor 与安排 expiry；replay/store 错误向上返回。
- `load_pending_sessions` 按 session 捕获安装错误并记录 `session_id`，继续恢复其余 pending sessions；返回计数只包含成功安装项。
- 故障回归验证坏 session actorless、失败安装不启用其 grant、显式加载再次 replay 失败；同批健康 session 与其 grant 正常恢复。
- 通过：定向回归；`cargo fmt --all -- --check`；`cargo test --workspace --locked`；`cargo check --workspace --locked`；`cargo clippy --workspace --locked -- -D warnings`；crate dependency inventory；ADR index；`git diff --check`。
- 无 schema、IPC、event payload 或用户数据变更。全局 Security apply 对 durable grants 的重应用契约依 ADR 0402 保持不变；坏 session 没有定时或退避自动重试，后续显式加载/恢复调用可重试。
