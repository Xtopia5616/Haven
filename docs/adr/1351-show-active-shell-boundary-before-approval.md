# ADR 1351：在 Shell 授权前显示当前执行边界

## 状态

已实现 UI 与本地 fail-closed 状态覆盖；真实 HCS guest、配额超额写入和目标 Windows build/镜像验收仍按 ADR 1260 保持为开放发布 Gate。本机没有运行中的 Haven broker，且无法在当前权限下读取 Windows 可选功能状态，因此未完成该环境验收。

## 背景

Shell 授权弹窗此前只显示操作摘要和授权期限，用户无法在决定前区分本机子进程与 Workspace Write 的 HCS guest，也无法看到 broker 的 quota gate 状态。设置页的安全设置展示的是可编辑草稿，部分应用失败时还可能与当前进程实际采用的策略不同。

## 决定

- `AuthorizationPort` 暴露只读 `SecurityBoundarySnapshot`，仅含当前生效的 sandbox mode 与 network policy，不返回工作区路径或可变内部配置。
- Tauri `get_shell_execution_security_status` 返回有效策略和 Shell 所需的 broker quota gate。Windows 查询复用现有 authenticated status pipe，不启动 service 或 guest；SCM/pipe 错误映射为 `unknown`，非运行状态映射为 `service_unavailable`。
- 设置页标明展示的是进程当前策略，并单独提示未保存草稿、读取错误与关闭的 Workspace Write gate。Shell 确认弹窗对 `tool_name=shell` 显示相同摘要，再显示允许选项。
- 用户文案说明 Full Access 子进程在本机以当前 Windows 用户权限访问文件与网络；执行工作区只决定默认目录。Workspace Write 使用 HCS guest 和隔离输出审阅，缺少配额证明时拒绝执行，不退回本机 Shell。文案同时明确启动 quota probe 状态不能代替目标环境验收。
- Renderer 不接收命令正文、cwd、workspace 路径、broker 错误文本或 HCS 诊断。状态读取失败时显示未知；实际执行仍由后端授权与 HCS gate 决定。

## 替代方案

- 在 UI 直接把 SettingsSnapshot 草稿当作运行状态：部分应用失败时会误报，拒绝。
- 仅展示 SCM `Running`：不能证明 startup quota probe 通过，拒绝。
- 在弹窗查询时创建 guest 或重跑配额探针：会产生副作用并重复昂贵工作，拒绝。

## 影响与迁移

新增只读 Tauri 命令和生成 IPC 契约；不改变授权规则、Shell 执行协议、配置格式、数据库或持久数据，不需迁移或重置。状态查询不授予执行权，现有 Shell authorization proof 与 fail-closed gate 仍是执行依据。

## 验证

- 单元测试覆盖活动授权策略快照，以及 broker 未运行、配额未证明、认证查询未知和 attested 状态映射。
- UI 测试覆盖 Full Access、Workspace Write、broker 未就绪、缺少状态和网络策略文案。
- 本机真实 HCS 验收未运行：当前没有 HavenContainerBroker service，系统 HCS 功能查询需要提升权限。状态查询和自动测试不会关闭 ADR 1260 的发布 Gate。

## 回滚

移除状态 DTO、Tauri 命令、授权快照接口及两个 UI 状态展示即可；不需要数据回滚或重置。执行授权和 HCS quota gate 无需随状态展示回滚。
