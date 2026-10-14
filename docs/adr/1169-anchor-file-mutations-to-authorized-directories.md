# ADR 1169：将文件变更锚定到授权目录句柄

## 状态

已采纳并实施。

## 背景

ADR 1167/1168 统一了后端执行根、写入授权和默认 Shell 工作目录，但 Files 变更在通过路径策略检查后仍可能通过普通绝对路径重新打开目标。若检查与使用之间目录被替换或重解析点改变，路径预检本身不能稳定代表实际写入位置。

## 决策

- Platform 提供 `DirectoryCapability`：从本地绝对策略根打开并持有目录句柄，所有受约束文件操作使用相对该句柄的路径。拒绝绝对路径、父目录分量、重解析点和非普通文件目标；缺失的父目录只能在已持有的根下创建。
- 每次 Files 写入 attempt 的 `FileWritePolicy` 从授权 proof 绑定的 policy revision 捕获工作区、`writable_roots` 和工具 `allowed_paths` 的目录句柄。捕获前重新验证 proof，并在同一策略 revision 下读取 sandbox 与路径策略；若策略已变化则 fail closed。
- Files 的 write、create_dir、edit、patch、copy、move、delete 均通过该策略解析源和目标。edit/patch/copy 从 capability 下读取；原子替换先在已验证父目录句柄下创建 `.file-*.tmp`，再相对同一目录提交，失败时清理 staging 文件。
- `workspace_write` 的默认工作区能力复用进程级 `ExecutionWorkspace` 已持有的根句柄。显式 writable root 和工具 allowed path 在本次授权执行中打开并持有。`full_access` 仍保留显式 ambient 权限，但该 attempt 同样将操作锚定在解析时打开的最近现存目录句柄下。
- Windows 工作区根句柄不共享 delete 权限，使根目录在 capability 生命周期内不能被重命名。替换时再次打开目标父目录并与 capability 父句柄核对文件身份，再通过相对该句柄的原生 rename 提交。
- 此能力只覆盖 Files 工具的文件系统操作。它不限制 Shell、Skill、MCP 子进程、网络或其他进程级路径访问；`cwd` 和 Job Object 不是 OS filesystem sandbox。scratch 管理与工作区切换生命周期另行处理。

## 替代方案

- 保留 reparse-aware 路径预检并继续按绝对路径写入：拒绝，因为目录项仍会在检查后重新解析。
- 让 Files 直接使用进程 ambient filesystem authority：拒绝，因为会绕过 workspace 与 `allowed_paths` 的 capability 范围。
- 把本切片称为进程级 OS sandbox：拒绝，因为普通进程和子进程仍可使用各自 ambient authority。

## 影响与迁移

不改数据库、配置、IPC 或持久化格式，无需迁移或重置。Files 写入现在要求带授权 proof 的目录能力；生产调用缺失该上下文会 fail closed。`full_access` 仍须由现有授权策略显式授予。取消只能阻止尚未开始的变更，文件系统已接受的原子操作不能回滚。

`expected_hash` 仍是在原子替换前读取并比较的并发保护，不构成与替换操作合并的文件系统 CAS；其既有语义未在本 ADR 扩大。

## 验证

- Platform 的目录能力测试覆盖相对根的原子创建/替换、拒绝父目录逃逸，以及 Windows 上根句柄存活时阻止重命名并在释放后恢复重命名。
- Tools 测试覆盖授权目录内写入、授权根外拒绝、策略 revision 变化后拒绝旧 proof，以及现有 Files 变更行为。
- `cargo fmt --all -- --check` 通过。
- `cargo check --locked --offline -p haven-tools` 通过。
- `cargo clippy --locked --offline -p haven-platform -- -D warnings` 与 `cargo clippy --locked --offline -p haven-tools -- -D warnings` 通过。
- `cargo test --locked --offline -p haven-platform`：10 passed；`cargo test --locked --offline -p haven-tools`：900 passed、2 ignored；MCP integration：7 passed。
- 当前 Windows 环境运行了 Windows 专用句柄测试；仍需在发布验收中覆盖更广泛的 reparse-point、目录替换竞态和 OS 隔离边界。此 ADR 不关闭 Windows filesystem sandbox 发布 Gate。
