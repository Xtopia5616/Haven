# ADR 1421：在 WorkspaceWrite 下隔离 Lifecycle Hooks

## 状态

已实现；当前 Windows 环境的 MXC Hook 进程端到端用例通过。完整发布签核仍按路线图 §5.1 跟踪。

## 背景

用户级和受信任项目级 Lifecycle Hooks 以配置命令运行。原有规则只允许 `FullAccess` 且网络为 `Open` 时启动，导致 WorkspaceWrite 无法使用 Hook。仅用 Job Object 管理进程树不能限制它访问用户文件或网络；同时，将工作区根当作通用临时写入根会绕过 Hook 的只读模式和路径范围。

## 决定

- `FullAccess` 仅在网络为 `Open` 且全局与 `lifecycle_hook` 工具都没有域名白名单时启动；任意子进程无法被域名白名单可靠约束。命令在宿主用户权限下运行并由 `ProcessContainment` 管理进程树。
- `WorkspaceWrite` 只在 Windows 上通过 MXC 启动 Hook。授权 owner 从活动执行工作区、全局文件边界、`lifecycle_hook` 工具文件策略和运行时只读依赖编译进程范围；不得额外把 cwd 当作 scratch 写入根。网络保持关闭，与全局网络模式无关。
- `ReadOnly`、`FullAccess` 下非 `Open` 网络策略或任一域名白名单、非 Windows WorkspaceWrite、工作区或策略根不可用，以及 MXC probe/spawn 失败都拒绝启动。禁止回退到宿主进程。
- 命令在活动执行工作区作为 cwd。MXC 分支将 `TEMP`、`TMP` 和 `TMPDIR` 指向该工作区；命令及所需脚本必须位于授权读范围内。既有取消、五秒事件期限、输出限制、进程树回收和安全策略撤销继续适用。
- PreToolUse 的执行失败继续 fail closed；通知事件记录静态诊断并继续主操作。用户级与受信任项目级 Hook 顺序、事件 payload 和 IPC 契约不变。

## 替代方案

- 只允许 FullAccess：拒绝，WorkspaceWrite 下的 Hook 可通过 MXC 限定文件写入并离线运行。
- 用 Job Object 在 WorkspaceWrite 下直接运行：拒绝，Job Object 不限制文件和网络访问。
- 把 cwd 作为独立 scratch 写根：拒绝，它会越过 Hook 的只读和全局路径范围。
- MXC 失败时退回宿主进程：拒绝，会扩大权限且与用户选择的沙箱不符。

## 影响与迁移

WorkspaceWrite 用户可在 Windows 上使用 Hook，Hook 写入遵守当前执行工作区与文件策略且不能联网。FullAccess 的域名白名单会阻止 Hook 启动，避免把不能强制的白名单误作任意进程的网络限制。Hook 所需的可执行文件和脚本必须能从该沙箱的工作区/运行时只读范围读取。设置页会说明 FullAccess 与 WorkspaceWrite 两种执行边界。配置、数据库和 IPC payload 不变，无需迁移或重置。

## 验证

- `cargo fmt --all -- --check` 通过。
- `cargo test --locked -p haven-tools --lib hook -- --test-threads=1`：12 passed；覆盖 WorkspaceWrite 的离线与工作区写范围、运行时依赖只读、只读工具策略、全局路径范围拒绝、ReadOnly 拒绝、FullAccess 网络与域名限制、过期授权，以及真实 MXC Hook 命令启动与工作区写入。
- `cargo clippy --locked -p haven-tools -- -D warnings` 通过。
- `corepack pnpm run check` 通过；`corepack pnpm run test:run`：170 files / 1519 tests passed。

## 回滚

可恢复 WorkspaceWrite 下拒绝 Hook 启动；不得删除 spawn 前实时授权、运行中撤销、取消/超时清理或 FullAccess 的 `Open` 网络门槛。无需重置持久数据。
