# ADR 1199：在子进程启动点重检实时授权策略

## 状态

已实现。`AuthorizationEngine` 为 proof-verified tool attempt 派生短时 `ExecutionSpawnAuthorization`；前台 Shell 与后台 Shell ToolRun 在创建 OS 子进程前取得 spawn permit。

## 背景

工具执行 proof 在每次 attempt 开始时重检当前策略。proof 校验和实际子进程创建之间仍有准备工作：Shell 要建立进程树 owner；后台 Shell 还要等待 admission、写入 durable ToolRun 并注册运行态。用户可能在这些等待期间切换 execution workspace、sandbox、network policy、permission mode 或 deny rule。只在 attempt 开始校验会使旧授权穿过这一段等待。

策略配置由 `AuthorizationEngine` 的单个 Tokio `RwLock<SafetyConfig>` 持有。所有安全配置、权限 grant、工具设置和 workspace 更新由该 owner 递增 `policy_revision`；workspace 更换另增 generation。proof 本身绑定 revision、generation 经间接 revision 校验、输入和有效期。

## 决定

- 从当前 proof 构造内部 `ExecutionSpawnAuthorization`，只保存授权 owner 的 `Arc`、proof revision、执行 workspace generation 和过期时间；它不向工具暴露可变配置或通用授权接口。
- 在真实进程创建前，`acquire` 获取当前 `SafetyConfig` 的读锁，并重检 proof 有效期、policy revision 和 workspace generation。任一不匹配时拒绝 spawn。permit 持有读锁直到同步 OS spawn 返回，因此策略更新要么先完成并使 permit 失效，要么等待进程创建返回后生效。
- 前台 Shell 在构造命令、设置环境和创建 Job Object 后，紧邻 `tokio::process::Command::spawn` 取得 permit。策略不匹配通过结构化 `Permission` 错误返回，不做自动重试。
- 后台 Shell 将 spawn authorization 留在当前进程内的 admission 请求中，不写入数据库。它先保留现有持久化/注册顺序，再紧邻 `spawn` 取得 permit；若策略已变化，删除刚创建的 ToolRun 注册并返回权限失败，不启动子进程。ToolRun 重启恢复不会重放该 transient permit。
- 全局 `process_spawn_lock` 与该 permit 分工不同：前者只防止并发创建继承句柄，后者使 policy 更新不能和授权检查/进程创建交错。持锁顺序固定为 policy read permit 后取得全局 spawn lock；二者都只覆盖同步 spawn 临界区。
- permit 只定义启动时授权线性化点。子进程启动后的策略更改不会由本 ADR 杀死运行中进程；运行中撤销与 workspace 销毁协调留给进程 owner lifecycle 设计。创建 AppContainer workspace 的有界快照也必须在 permit 之外完成，并在真实 CreateProcessW 前重新取得 permit。

## 替代方案

- 仅在 attempt 开始校验 proof：拒绝，后台 ToolRun 的等待、持久化和注册可令策略变化先于 OS spawn。
- 把整个工具执行期间的授权配置读锁持有不放：拒绝，会使长时间运行的 Shell 阻塞 Settings、权限撤销和 workspace 切换。
- 只在 spawn 前再次读取 revision 后立即释放锁：拒绝，revision 检查与 OS 创建之间仍有可并发改变策略的窗口。
- 把 permit 序列化到 ToolRun：拒绝，permit 是进程内、短时且持锁的能力，不能在重启后复用。

## 影响与迁移

增加 Tools 内部的 proof 派生短时 permit，不增加配置、IPC、数据库或持久字段，无需重置或兼容迁移。后台 ToolRun 因策略变化拒绝启动时，会回滚本次注册；已运行的 process tree 与 sandbox workspace 生命周期不在本切片内。

## 验证

- `AuthorizationEngine` 测试验证配置更新先于 spawn permit 时 fail closed、workspace generation 变化使 permit 失效，并验证 permit 持有期间写策略的更新等待，释放后旧 context 不可再启动。
- Tools 全量测试覆盖前台/后台 Shell、ToolRun 回滚与相邻授权/工作区契约；workspace 检查、严格 Clippy 和格式检查作为本切片门禁。
- 依赖真实 AppContainer 的最终业务路径、后台 ToolRun 策略拒绝的持久回滚哨兵、启动后撤销进程以及跨崩溃 stale workspace 清理仍由后续切片验收。

## 回滚

移除 `ExecutionSpawnAuthorization` / permit 及 Shell caller 的最终重检，恢复仅按 attempt 开始 proof 运行；移除本 ADR 的索引和路线图说明。没有持久数据需要恢复。
