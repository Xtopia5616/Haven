# ADR 1419：通过受控 Git 入口写入仓库元数据

## 状态

已实施；自动化门禁通过。真实 Git commit、hook/filter 后代和 MXC 网络行为仍需纳入 Windows 发布验收。

## 背景

WorkspaceWrite 会把写入根中的 `.git` 等版本控制元数据设为只读。直接放开普通 Shell 的 `.git` 会允许任意命令和后台派生进程修改 refs、配置或 hook，因此不能作为 Git 工作流的修复。

## 决定

- 新增高风险 `git` 工具，直接以 argv 调用系统 Git，不经 Shell。仅接受固定的内建子命令；禁止仓库/工作区/可执行文件作用域覆盖参数。`config` 仅可在仓库本地读取或设置 `user.name`、`user.email`。
- Git 操作声明为 WorkspaceWrite。全局与 `git` 工具级 path roots、write mode、denied paths、protected-read-only paths 和有效写入根均由执行授权 owner 校验。Shell 编译器保留自己的 `shell` 配置，不能因为 Git 能力改变 `.git` 保护。
- WorkspaceWrite 仅在仓库 `.git` 是真实目录、位于有效写入根内且未与显式 deny/read-only 规则重叠时，才从自动生成的 VCS protected-read-only 列表中移除该目录。linked worktree 的 `.git` 文件与外置 metadata 不支持；MXC 或请求编译失败时拒绝启动，不回退宿主进程。
- Windows WorkspaceWrite 用 MXC ProcessContainer 启动 Git，项目写入范围遵循已配置的 workspace 根，`.git` 是唯一额外的受控 metadata 例外，调用 scratch 独立且有界。Git hooks、filters、remote helpers 与其它后代进程继承同一 MXC 文件和网络策略。
- WorkspaceWrite 本地命令强制离线；`fetch`、`ls-remote`、`pull`、`push` 属于 opaque network，只有网络策略为 Open 且没有域名约束时可执行，MXC 仅允许公网出站。FullAccess 中所有 Git 子命令均视为 opaque，因为 hook/filter 可能联网；因此需要 Open。ReadOnly 拒绝 Git。
- Git 调用清空继承环境，仅传递运行基线和本次 scratch。系统/全局 Git 配置不加载，Git 临时配置放在 scratch；不继承代理变量或凭据。
- 取消、执行策略变更、输出预算、scratch 容量与既有进程生命周期控制适用于整个 Git 子进程树。scratch 上限 256 MiB、100,000 项。

## 替代方案

- 让 Shell 对 `.git` 目录例外：拒绝，Shell 是任意命令入口，无法只授予 Git 元数据写能力。
- 对全部 Git 命令放开 `.git` 或允许任意 `git config`：拒绝，扩大了任意 metadata 写入与配置驱动执行面。
- 仅依赖 Job Object 或环境代理：拒绝；它们不提供文件或强制网络隔离。
- 通过 MXC 受控代理实现域名策略：本 ADR 不引入此能力；域名策略下仍拒绝 opaque Git 网络调用。

## 影响与迁移

新增 `git` 工具供 Agent 和 ToolRun 使用；后台执行仍经同一授权执行入口。配置、IPC、数据库 schema 均不变。FullAccess 下本地 Git 命令也需 Open 网络策略，以覆盖 hook/filter 的不透明网络能力。WorkspaceWrite 仅支持 Windows MXC；普通 Shell 的 `.git` 保护保持不变。

## 验证

- `cargo test --locked -p haven-tools git_`：6 passed；覆盖 Git 路径策略只解除当前 `.git` 自动保护、显式 deny/read-only 仍生效、工具级写策略、参数/子命令白名单、仓库本地身份配置约束及 sandbox/network contract。
- `cargo test --locked -p haven-tools test_local_tool_security_matrix_gates_every_risk_bearing_case`：1 passed；新 `git` builtin 被注册并纳入授权风险矩阵。
- `cargo check --locked -p haven-tools`、`cargo clippy --locked -p haven-tools -- -D warnings`、`cargo fmt --all -- --check`、ADR index（1,316 条）与 `git diff --check` 通过。
- 自动化测试没有通过真实 Windows Git/MXC 进程执行 commit；实际 hooks、filters、remote helper 与文件/网络边界由 Windows 发布验收补齐。

## 回滚

可移除 `git` 工具并继续拒绝 WorkspaceWrite Git 修改；不得改变普通 Shell 的 `.git` 保护或加入宿主进程 fallback。没有持久数据格式迁移。
