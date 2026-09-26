# ADR 0373：Tools Action admin writers 并发边界审计

- 状态：已采纳（2026-09-26）
- 基线：HEAD `5bc13eb`；开始时工作区干净
- 范围：background/scheduled action 的模型工具写入口、`ActionService`/`ActionStore` 状态写入边界及 Tauri action 命令
- 关联：ADR 0305、0317、0325、0332、0334、0335、0338、0343、0344、0348、0352、0353、0361、0374

## 背景

架构路线图已有 ActionService 状态转换、终态 CAS/outbox、claim lease、重试和 UI 投影审计，但还没有单独核对 action 管理写入口是否共用一个运行时 owner，以及 Tauri、model-facing tool、定时触发和后台完成是否各自维护写锁或状态。此次审计只处理 Tools action 管理边界；不扩展为完整 Job lifecycle、session orchestration、Settings apply 或全局 IPC/codegen 重构。

## 证据与结论

### Owner 与写入口

- `ToolRuntime::new` 为应用构造一个 `Arc<ActionService>`。`ToolsManager::ToolServices`、`AgentLayer` 和 `AppState` 取得该共享服务；bootstrap 在同一服务上安装生命周期 sink。没有分别供 Tauri、Agent 或工具使用的第二份 action board/service。
- `ActionService` 拥有进程内 action board、`spawn_gate`、terminal-transition guard、completion bus、生命周期 sink 及与持久化端口的绑定。production action row/outbox 写入经 `ActionStore` 进入 Memory repository；Tools 不直接持有 raw `Database`。
- model-facing `actions` 工具提供按当前 session 限定的 list/status/cancel；`schedule` 工具提供按当前 session 限定的 set/list/cancel。`schedule.set` 只创建新的 action 并返回新 ID，没有更新既有 action 的 admin 操作。
- scheduled trigger 是 ActionService timer/dependency worker 对 waiting row 执行 durable `Waiting → Running` CAS 后交给 AgentLayer；后台 shell admission、scheduled fire 和完成/失败回报都进入同一 ActionService。没有独立的 Tauri 手动 trigger 或通用 status-set command。
- Tauri action 命令读取 action board/history、以 `kind` 限定取消，并只允许删除 terminal history。`list_action_history` 和 `delete_action` 已登记，但本次源码审计未发现 UI 调用者；当前 UI 只经 typed helper 调用 `list_actions` 与 `cancel_action`。
- `ToolConcurrency` 是 ReAct 工具批次的调度 metadata，不协调 UI/Tauri 写入、定时 worker 或不同 session 的调用，不能作为跨入口的互斥机制。

### 并发与待决边界

- `ActionStatus::can_transition_to` 和 `action_terminal::can_claim_terminal` 是状态/终态仲裁的集中策略。持久化 status 变化由 kind/current-status 条件 CAS 竞争；scheduled fire、取消和终态提交仍由 ActionService 按各自路径管理。background terminal row 与 completion outbox 在同一事务提交。
- admission 与部分 scheduled mutation 经 `spawn_gate` 串行；终态持久化 worker 使用 terminal-transition guard。terminal history 删除在本 ADR 审计时只取得 `spawn_gate`，不能与所有 background terminal commit/retry 和 scheduled terminal retry 形成完整的共同临界区；completion ack 前拒绝删除及 ack/delete writer race 随后由 ADR 0374 收口。
- completion outbox 对 `actions.id` 有 `ON DELETE CASCADE` 外键。产品已确定：terminal history 在 completion ack 前拒绝删除；因此删除必须与 outbox ack 使用同一 SQLite writer 原子边界，不能仅靠应用层先读 pending 再删除。该 guard 与竞态回归见 ADR 0374。
- completion delivery 仍以 transcript projection 后 ack 为主路径；无 owner 结果可由 Agent 收口，但须在同一 SQLite 写入中确认 action 与 outbox 都仍无 owner。迟到绑定会重新打开该 outbox，避免 stale 无 owner 事件吞掉 session 结果（ADR 0374）。该例外不把 completion 从 action history 中拆成独立 owner。

## 后续产品决策（2026-09-26）

后台任务与定时任务在完成记录、任务卡和 transcript 投影上采用统一格式；具体内容仍可保留
background/scheduled 的类型细节。该决定只统一投影契约与用户可见结构，不改变现有状态机、completion
delivery/retry、取消或 X12 语义，具体实现留给独立 action/UI 投影切片。

## 决定

1. 保持一个共享 `ActionService` 为 action 的运行时 owner，保持 `ActionStore` 为持久化写边界；不引入第二个 Tauri/tool writer、通用状态 setter 或把 `ToolConcurrency` 当作跨入口锁。
2. 保持现有 kind、status、lease、scheduled-running、取消、通知、错误、schema、ID、CAS/outbox、事件和 X12 语义。此次不改写入策略，也不添加新的重试、去重或状态。
3. Terminal history delete 在关联 completion ack 前必须返回拒绝；删除操作先在同一数据库写路径重建缺失 outbox，再以 `delivered_at IS NULL` 条件阻止级联删除。ack/delete 竞态不得产生删除成功而 ack 失败；实现和确定性测试见 ADR 0374。
4. 仅在 App command projection 处共用 ActionRow→ActionEvent mapper；用显式 projection variant 保留 board history 与 history command 现有不同字段和 preview 行为，并验证 `tool_args`、prompt、log path 等内部字段仍不外泄。

## 验证

- `cargo fmt --all -- --check`
- `cargo test --workspace --locked`
- `cargo check --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `pwsh -NoProfile -File scripts/check-ipc-contracts.ps1`
- `git diff --check`

无 UI 文件变化，因此不运行 UI gates。Rust gates 的实际结果以本提交记录为准。

## 回滚

回滚安全 projection mapper 共用及本 ADR/索引/路线图/架构记录即可；不需要数据库、配置或用户数据重置。
