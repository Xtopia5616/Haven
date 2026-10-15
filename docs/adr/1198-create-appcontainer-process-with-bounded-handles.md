# ADR 1198：通过显式句柄白名单启动 AppContainer 进程

## 状态

已实现为 `haven-platform::appcontainer_process::AppContainerCommand` 与 `AppContainerChild`；Windows 隔离探针已改用该启动器。Shell、Skill、MCP 与 ToolRun 还没有将业务执行切换到 AppContainer。

## 背景

ADR 1195–1197 已定义 canonical `ExecutionWorkspace`、有界文件快照、绑定 workspace generation/policy revision 的私有 `AppContainerWorkspace`，但生产进程创建仍没有消费这个 workspace 的实现。Windows AppContainer 需要通过 `SECURITY_CAPABILITIES` 创建受限 token；子进程还必须先进入现有 Job Object，再恢复初始线程，才能让首条指令及其后代都处于进程树边界内。

产品适配器需要 stdin/stdout/stderr。直接继承父进程全部 handle 会让任意一个工具子进程拿到其他会话、server 或文件流的 handle；仅设置 `STARTUPINFO` 标准句柄也没有限制继承集合。Windows 提供 `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` 供 `STARTUPINFOEX` 精确列出需要继承的 handle。显式环境块还必须维护盘符当前目录伪变量，否则 CreateProcess 不会自动传播 `=C:` 等信息。[Create processes](https://learn.microsoft.com/en-us/windows/win32/procthread/creating-processes)、[CreateProcessW](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessw)、[UpdateProcThreadAttribute](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-updateprocthreadattribute)

## 决定

- `AppContainerCommand` 要求绝对 executable path；参数以 Windows UTF-16 命令行规则编码；工作目录只能是 sandbox 根下的相对目录，并逐层拒绝 junction/reparse point。默认环境为空，只有调用者显式追加的变量会进入子进程；变量名限定为 ASCII 标识符、忽略大小写合并且后写覆盖先写。环境块只额外生成当前 sandbox 盘符的 `=X:` 目录项，不复制其它盘符的用户路径。
- `spawn` 创建三条匿名 pipe，只向子进程传递 stdin-read、stdout-write 和 stderr-write 三个 inheritable handle；父进程端清除 inherit flag。`PROC_THREAD_ATTRIBUTE_HANDLE_LIST` 与 `STARTF_USESTDHANDLES` 同时设置，`bInheritHandles=TRUE` 仅服务于这个明确列表。属性列表同时附带 workspace package SID 和空 optional-capability 列表，因此该进程没有可选网络 capability。
- 所有 Haven 生产进程创建共用 `process_containment::process_spawn_lock`。锁从 AppContainer pipe handle 创建，到 `CreateProcessW` 返回且父进程关闭三个 inheritable child-side handle 后释放；Shell、后台 Shell、Skill runner/setup、MCP stdio、应用路径打开、shell 检测和 schtasks 调用也在实际 spawn 时持有同一把锁，避免另一个 Haven child 在 inheritable handle 窗口内启动。
- `CreateProcessW` 使用 `CREATE_SUSPENDED`。成功后必须先将返回的 process handle 绑定到 kill-on-close Job Object、再恢复初始线程；Job Object 建立或 attach/resume 失败时，仍 suspended 的进程被终止。
- `AppContainerChild` 同时持有 process、标准流、Job Object 与 `AppContainerWorkspace`。root 进程退出后，owner 终止仍存活的后代，并查询 Job Object 直到活动进程数归零；`Drop` 也会执行相同的停止与等待。若 5 秒内无法确认进程树为空，owner 有意保留 workspace/profile，避免删除仍可能被进程使用的目录；后续 stale-workspace lifecycle slice 负责定义恢复/回收策略。
- 启动器只执行平台级 token、句柄与生命周期操作，不自行授予工具授权。产品 caller 仍必须从当前授权 owner 取得 workspace，在启动前核验当前 deny、policy revision 和 workspace generation；输出文件后续必须形成有界 manifest，并按逐路径 proof/capability 晋升。上述检查和结果晋升尚未接入，所以 WorkspaceWrite 下 Shell、Skill、MCP 继续 fail closed。

## 替代方案

- 复用 Tokio `Child` 并通过标准 `Command` spawn：拒绝，`std::process::Command` 没有创建 AppContainer token 所需的属性入口。
- 将 `bInheritHandles=TRUE` 配合空 handle list：拒绝，会使 stdin/stdout/stderr 不可用，并可能暴露父进程已有的 inheritable handle。
- 继承整个环境：拒绝，可能传递 provider credential、Python 路径和运行时 override；只传调用方明确提供的白名单项。
- child 退出就删除 workspace：拒绝，后代可能仍在 Job Object 中并持有 cwd/文件；先停止并确认整个 Job 为空。

## 影响与迁移

新增 Windows 专用 platform launcher、Job Object drain API 和跨适配器进程创建锁，并将 Windows probe 改为通过正式 launcher 创建。没有配置、IPC、数据库或持久格式变化。现有业务适配器尚未消费 launcher，故当前用户执行语义保持不变；也不得据此宣称 Shell、Skill 或 MCP 已经受到 OS sandbox。

## 验证

- Windows AppContainer probe 使用 production launcher：验证 sandbox 内可写、workspace 外读写失败、无 network capability 时 loopback 连接失败、父进程只向子进程传入显式 stdio handles、未允许的环境项缺失、显式环境项可读、root 退出后 Job tree 清空且 workspace 被回收。
- launcher 单元覆盖绝对 executable 要求、环境变量大小写合并、Unicode/引号/反斜线参数编码、workspace cwd 路径拒绝与环境块盘符伪变量。
- `cargo test --locked -p haven-platform`、工作区 locked check、严格 Clippy、格式检查、ADR 索引与 crate 依赖检查作为本切片门禁。
- 同用户进程对临时目录/工作区进行恶意并发替换的 OS adversary 场景、应用 caller 的 live authorization recheck、外部子进程启动器以及崩溃后 scratch 回收仍待后续切片覆盖。

## 回滚

移除 launcher、共享 spawn lock 和 probe 迁移并恢复之前的测试 helper，再移除此 ADR/索引/路线图状态。此切片无持久数据需要迁移。
