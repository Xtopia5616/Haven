# ADR 0161：ApplicationRuntime 统一应用生命周期

- 状态：accepted
- 日期：2026-09-15
- 范围：`haven-app-binary`、`haven-agent`、`haven-tools`、`haven-input` 的后台任务与退出路径

## 背景

应用组合根过去在 `AppState::new`、Tauri bootstrap、录音命令和领域 crate 中分别
`spawn` 后台任务。任务没有共同 owner，取消语义也不一致：部分任务没有 token，技能
监听器内部再次 detach，退出事件只暂停数据库中的 session，而没有等待消费者、录音
设备、action 进程和定时器完成 teardown。这样会产生退出竞态、测试泄漏和重启后残留
`running` 状态。

## 决定

1. `haven-app-binary::runtime::ApplicationRuntime` 成为应用组合根的生命周期 owner。
   它集中持有 db、tools、session supervisor、agent、input pipeline、desktop shell、
   config service 和日志 reload handles，并登记 app-scoped `JoinHandle`。
2. runtime 提供一个根 `CancellationToken`。通过 runtime 注册的任务获得 child token，
   任务必须在等待、轮询和长操作边界响应取消；来自托盘和 global shortcut 的线程也
   通过 runtime 保存的 Tokio handle 注册任务，不能自行 detach。
3. `shutdown`/`teardown` 是幂等入口。顺序固定为：停止输入采集，清理会话 actor，
   停止 ActionService 的进程/定时器，关闭 MCP clients，最后取消并 join app-scoped
   tasks；超过宽限期的任务才 abort。Tauri `RunEvent::Exit` 和测试 teardown 共用这
   一条路径。
4. 领域资源继续由所属 crate 释放：`InputPipeline::shutdown` 释放采集 engine 与
   VAD worker，`ActionService::shutdown` 终止运行中的后台进程并停止内存定时器，
   `SessionSupervisor`/Agent consumers 使用 runtime 子 token。待触发的 scheduled
   action 行保留为 durable pending 状态，供下次启动恢复，不因正常退出被静默取消。
5. `AppState` 只保留录音、bootstrap 和 UI confirmation 等 Tauri 瞬态；通过对
   `ApplicationRuntime` 的稳定句柄访问现有 command 依赖，避免再次建立平行 service
   owner。数据库没有新增 close API，所有依赖它的 worker 先停止后由 runtime 引用释放。

## 替代方案

- 继续在各调用点直接 `tokio::spawn`：改动最小，但无法枚举 owner、统一取消或在退出
  时 join。
- 只在 Tauri `RunEvent::Exit` 中广播一个全局信号：不能覆盖命令/测试 teardown，且
  无法保证领域资源在 token 后按依赖顺序释放。
- 把所有 worker 都搬进 app crate：会让 agent、tools、input 反向依赖宿主，破坏
  crate 边界；因此保留领域 worker 的本地实现，由 runtime 负责应用级编排。

## 影响

- 应用级任务有可追踪 owner、取消边界和 join 语义；窗口关闭、setup 失败和测试退出
  不再依赖未持有的 detached handle。
- `ActionService` 的 pending scheduled rows 在退出后可恢复；运行中的 background
  action 明确进入 cancelled 终态，避免 ghost running rows。
- `ApplicationRuntime` 仍通过 `AppState` 暴露服务句柄，当前 Tauri 命令的 wire/API、
  数据库 schema 和配置 schema 不变。
- provider 调用、标题生成和录音循环等更细粒度任务仍由其领域 owner 管理；它们必须
  继续响应所属 session/input token，后续若提升为应用级长任务应迁移到 runtime registry。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `cargo test --locked -p haven-app-binary --lib`
- `cargo test --workspace --locked`
- `corepack pnpm --dir ui run check`
- `corepack pnpm --dir ui run test:run`
- `corepack pnpm --dir ui run build`

新增回归覆盖 runtime shutdown 的任务取消、资源 drop、重复 teardown 和关闭后拒绝
新任务；ActionService 覆盖关闭后 scheduled timer 不再触发及拒绝新 scheduled work。

## 回滚与重置

这是进程内生命周期重构，不改变数据库、配置、IPC 或持久化格式；回滚代码即可，无需
用户数据重置。回滚时必须同时恢复 app bootstrap、退出 hook、录音收尾和领域取消接口，
不能只删除 `ApplicationRuntime` 而留下调用方的 token/owner 迁移。
