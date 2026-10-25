# ADR 1418：在 WorkspaceWrite 下隔离 LSP server

## 状态

已实施；自动化门禁通过。真实 LSP server 在 Windows MXC 下的端到端行为仍需随 Windows 发布验收复核。

## 背景

LSP server 是用户配置的宿主可执行文件。此前 `ProcessContainment` 只管理进程树，不限制其读取工作区外文件、写入或联网；因此 `WorkspaceWrite` 也只能拒绝启动，或者需要不准确地把 Job Object 描述为资源隔离。

## 决定

- `FullAccess` 保留 host process 行为，且要求 Open network policy；运行时授权契约继续把它标为 opaque network 与 `HostProcess`。
- Windows `WorkspaceWrite` 使用 MXC ProcessContainer。授权 owner 将工作区和运行时依赖编译为只读根，只授予本次进程独有的 LSP scratch 写根，并传入全关闭的 typed 网络策略。全局与 LSP 工具级 path roots、denied paths 和 protected-read-only paths 仍由同一 owner 编译并交给 MXC。
- LSP scratch 位于应用 runtime temp 下，每次调用独立创建；上限为 256 MiB 与 100,000 个目录项。超限或无法安全检查时取消 LSP 调用并终止子进程，退出时清理 scratch，启动清理也会删除崩溃遗留的 LSP scratch。
- FullAccess 的既有进程 containment、LSP 协议与结果处理保持不变。WorkspaceWrite 在非 Windows 或 MXC 启动失败时 fail closed，不回退到宿主进程。
- Tool cancellation 和已有 execution-policy revocation watcher 负责停止 LSP 子进程；MXC permit 在 spawn 完成前持有，避免授权边界与进程启动之间出现竞态。

## 替代方案

- 继续在 WorkspaceWrite 下拒绝 LSP：拒绝，因为 MXC 可以表达只读项目、独立 scratch 和离线进程策略。
- 把项目根设为可写，或仅用 Job Object 管理 server：拒绝，会把工作区写权限或本机用户的其它权限交给任意配置的 server。
- 通过环境代理声称禁网：拒绝，子进程可以绕过代理变量；本决策直接请求 MXC 离线网络 profile。

## 影响与迁移

WorkspaceWrite Windows 用户可继续使用 LSP 导航与诊断，但 LSP server 只能读授权范围、写本次 scratch 且不能联网。其它系统上的 WorkspaceWrite、ReadOnly，以及 FullAccess 下非 Open 网络策略仍拒绝 LSP 进程启动。只调整运行时授权与进程适配器，不改配置、IPC、数据库或持久化格式，无需数据重置。

## 验证

- 自动化测试验证 WorkspaceWrite LSP contract 为离线 `SandboxedHost`、FullAccess contract 仍是 opaque `HostProcess`，项目读取根只读且唯一写根为 scratch，stale proof 与不支持的策略被拒绝。
- `cargo check --locked -p haven-tools`、`cargo test --locked -p haven-tools lsp_`（11 passed）、`cargo clippy --locked -p haven-tools -- -D warnings`、`cargo fmt --all -- --check`、ADR index（1,315 条）和 `git diff --check` 通过。
- 自动化测试没有启动真实语言服务器；真实 MXC 的 LSP 进程读写、离线行为和取消由 Windows 发布验收继续覆盖。

## 回滚

若 MXC 不能在受支持主机上稳定启动配置的语言服务器，应恢复 WorkspaceWrite 下拒绝 LSP 的行为；不得回退到宿主权限。Scratch 内容是临时缓存，回滚会在启动清理时移除遗留目录；无持久数据需要重置。
