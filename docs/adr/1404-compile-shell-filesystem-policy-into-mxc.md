# ADR 1404：由授权 owner 将 Shell 文件策略编译到 MXC

## 状态

已采纳并实施。

## 背景

`WorkspaceWrite` 下的 Shell 由 MXC ProcessContainer 限制文件访问。此前授权阶段会分别校验全局与工具级路径，但启动请求只携带全局工作区/写入根；工具级 `write_mode`、`write_roots` 和 `path_roots` 没有完整进入 MXC。Shell 的 cwd 是普通调用中唯一声明的本地读取资源，进程策略还会在授权根之外自动添加 cwd 为只读目录。因此，命令可以通过 cwd 路径校验后，在全局根内但工具写入根外执行写入。

## 决定

- `ExecutionPolicyPermit` 是 MXC 文件策略的授权 owner。每次启动时，它从仍受 live policy 锁保护的快照生成文件根：全局 WorkspaceWrite 根与工具级 `Workspace` / `Roots` 写范围相交；全局和工具级 `path_roots` 同时约束读写范围。
- 全局和工具级 `read_only_roots` 表示可读但不可修改的现存文件或目录，保留其与宽泛读写根的嵌套关系。`denied_paths` 表示硬拒绝的现存文件或目录，优先级为 `denied > read-only > read-write`；更窄的只读授权不会重新暴露其上级拒绝路径。需要在读写授权内执行子路径拒绝时，必须由 MXC BaseContainer 原生 `deniedPaths` 能力承接，否则拒绝启动；任何读写根重叠（例如宽泛只读读取根中的较窄写入根，或写入根内的只读保护路径）都要求 MXC BaseContainer 的原生路径优先级，不允许降级到无法保留这些语义的 tier。
- Files 与 Shell 共用这一优先级。Files 在路径 capability 和打开句柄校验后，对显式 deny 拒绝读取、元数据访问和变更；对只读保护路径拒绝变更。Shell 将显式规则和受保护路径编译进 MXC 的 `readwritePaths` / `readonlyPaths` / `deniedPaths`。`.git`、`.hg`、`.svn`、`.bzr`、`.jj` 等写入根中的版本控制元数据自动作为只读保护路径；显式 deny 仍高于该保护。
- 路径身份按已打开文件系统对象归并，并持有句柄；同一对象的别名在同类路径集中只保留一项，跨授权类别按最高拒绝级别收敛。父子路径不因字符串相似而去重：只读子路径和拒绝子路径持续单独传递，组件边界与句柄身份共同用于匹配。
- 工具级 `ReadOnly` 生成空读写根，并把受全局及 `path_roots` 限制的工作区范围作为只读根。MXC 请求允许空 `readwrite_paths`。
- 经确认的 Shell 资源扩展可扩大对应读或写根，但仍受全局和工具 `path_roots` 约束；工具只读策略不会因扩展获得写权限。
- cwd 必须落在授权 owner 编译出的读或写根内；MXC 适配器不得隐式授予 cwd，也不再删改授权 owner 提供的重叠读/写根，只负责规范化、边界校验并提交给 MXC。
- Shell 的 Windows 运行时及命令搜索目录继续作为启动依赖显式加入只读根。没有适用授权根、cwd 无法纳入策略或 MXC 无法启动时，拒绝启动。

## 替代方案

- 只在调用前校验 cwd：拒绝，任意 Shell 命令仍可访问其它路径。
- 由 MXC 适配器自行追加或推断工作目录：拒绝，会把策略编译移出唯一授权 owner。
- 工具只读时仍授予全局读写根：拒绝，会使配置与进程能力不一致。

## 影响与回滚

新增可选配置字段 `read_only_roots` 与 `denied_paths`；现有配置缺少字段时使用空默认值，不需要数据迁移或重置。全局和工具级文件策略现在都限制 `WorkspaceWrite` Shell 的 MXC 进程与 Files 调用；只读工具策略可运行受限的只读 Shell。回滚时必须同时恢复原有拒绝范围和路径检查行为，不能重新由 MXC 隐式添加 cwd。

## 验证

- `haven-platform` 与 `haven-tools` 定向回归覆盖全局/工具写入根和路径根交集、工具级只读、cwd 越界或被拒绝、只读子路径、显式 deny 优先级、MXC 根集合不扩权及原生能力不足时 fail-closed。
- 在 Windows 11 Pro Insider Preview build 29683 的 MXC BaseContainer 主机实际运行并通过 `workspace_write_shell_is_confined_to_tool_write_roots_by_mxc`、`workspace_write_shell_applies_tool_read_only_policy_by_mxc`、`workspace_write_shell_enforces_readonly_subpaths_and_denied_paths`：工具 `write_roots` 外目录写入被拒绝；工具只读仍能读取但不能写入；保护子目录可读不可写，deny 子目录不可读。
- `cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings` 与 `cargo fmt --all -- --check` 通过；`scripts/check-ipc-contracts.ps1` 与 `scripts/check-adr-index.ps1` 通过。
- IPC 合约重新生成；`corepack pnpm run check` 与 `corepack pnpm run test:run` 通过（1500 tests）。
