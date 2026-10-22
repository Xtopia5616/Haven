# ADR 1368：用 MXC ProcessContainer 限定 WorkspaceWrite Shell

## 状态

已采纳并实施。

## 背景

ADR 1190 将 `WorkspaceWrite` 下的 Shell、Skill 与 MCP stdio 一律拒绝，因为 Haven 当时只有 Windows Job Object；它只约束进程树和生命周期，不能限制子进程文件或网络访问。MXC 提供进程级文件与网络策略，且当前 Windows 11 主机可以通过 MXC BaseContainer 验收工作区内写入和越界写入拒绝。

## 决定

- Haven 的桌面支持基线改为 Windows 11 24H2 或更新版本。MXC ProcessContainer 的产品最低构建为 24H2；此切片的网络策略会显式设置 host-loopback，因此 24H2/25H2 需安装 KB5124010 或更新累计更新，26H1 需安装 KB5124006 或更新累计更新。以 [MXC Windows OS-version support](https://github.com/microsoft/mxc/blob/main/docs/backends/process-container/os-version-support.md) 为兼容性来源。
- `haven-platform` 使用 MXC Rust SDK `mxc-sdk 1.0.1` 提供窄的异步进程接口。启动前针对完整请求执行 MXC policy probe，只有返回 `base-container` 且不需要 DACL 补强时才启动；探测失败、策略不兼容或 SDK 启动失败均拒绝，不回退到宿主进程。
- `WorkspaceWrite` 下只有 Shell 声明 `SandboxedHost` 并经 MXC 启动。有效 `writable_roots`（未配置时为 ExecutionWorkspace）是子进程的读写根；Windows 运行时、Shell 的 PATH 目录与其它 cwd 以只读权限加入。`TEMP`/`TMP` 指向首个授权写根。工作区根缺失或不可用时拒绝启动。
- MXC 网络策略按已授权的 Haven 网络设置映射：`Deny` 为进出站拒绝，`Ask` 在操作确认通过后允许，`Open` 允许；`Restricted` 仍由授权网关拒绝 opaque 网络操作。MXC 不替代 Haven 的风险确认与权限规则。
- `ReadOnly` 下 Shell 继续拒绝。`FullAccess` 下 Shell 继续按当前 Windows 用户权限启动并由 Job Object 管理。Skill、MCP stdio、LSP、Everything 和 lifecycle hooks 暂不使用该 capability，在 `WorkspaceWrite` 下继续拒绝或保持既有 FullAccess 限制。
- `Files` 的路径操作继续使用 Tools 授权和 `DirectoryCapability`；MXC 只约束受管 Shell 子进程，不把整个 Haven 进程或其它桌面程序变成 Windows 沙箱。

## 替代方案

- 继续拒绝所有 WorkspaceWrite 子进程：拒绝，因为当前 Windows 11 上 MXC BaseContainer 已能约束 Shell 文件与网络边界。
- 只用 cwd、环境过滤或 Job Object 放行 Shell：拒绝，这些机制不能提供文件访问隔离。
- MXC 不可用时回退宿主进程：拒绝，这会静默撤销 WorkspaceWrite 的文件范围。
- 同一切片迁移 Skill、MCP 与所有本机进程：拒绝，三者的持久进程、输入输出协议与生命周期不同；保持既有拒绝规则，待单独迁移各自的 owner 与验收。

## 影响与回滚

新增 Windows-only MXC 依赖、SandboxedHost 执行边界和设置文案。没有数据库、持久配置、Tauri IPC 或数据重置变化。回滚时恢复 Shell 在 `WorkspaceWrite` 下的硬拒绝并移除 MXC adapter；不能保留不受限制的宿主进程回退。

Windows 10 不再属于桌面运行与打包目标。更新版 MXC 与完整累积更新是 WorkspaceWrite Shell 的运行依赖；不满足条件时该 Shell 调用会 fail closed。

## 验证

- 在 Windows 11 专业版 Insider Preview `10.0.29683` 上运行 `cargo test -p haven-tools --locked workspace_write_shell_is_confined_to_configured_roots_by_mxc -- --ignored --nocapture --test-threads=1`：通过。MXC BaseContainer 允许工作区根内写 `inside.txt`，拒绝写向兄弟目录 `outside.txt`。
- `cargo test -p haven-platform --locked`：34 项通过；包括 Windows 参数转义与 MXC 请求不继承未筛选宿主环境的测试。
- `cargo clippy --workspace --locked -- -D warnings`、`cargo fmt --all -- --check`、`corepack pnpm run check` 均通过。`corepack pnpm run test:run` 通过 163 个测试文件、1396 项测试。
- `cargo test --workspace --locked -- --test-threads=1` 的单元和集成测试目标均通过（`haven-agent` 677 通过、1 忽略；`haven-tools` 928 通过、3 忽略）。首次运行的 App doctest 因 D 盘空间耗尽而找不到 `haven_agent` rlib；随后将 target 指向 C 盘、关闭调试符号后重跑 `cargo test -p haven-app-binary --locked --doc -- --test-threads=1`，通过（0 项 doctest）。
- `scripts/check-adr-index.ps1` 通过，索引覆盖 1268 条 ADR；`git diff --check` 通过。
