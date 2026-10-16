# ADR 1276：将 MCP stdio 配置绑定到受管 package

## 状态

已实现配置、MCP launcher port、snapshot/IPC 与设置 UI 的 typed package 绑定。当前生产 launcher 仍为本机 launcher；它会在 host spawn 前拒绝受管 package。受管 package 仍无法连接，直到 brokered stdio launcher 完成接线。

## 背景

ADR 1272–1275 已在受保护 image catalog 中登记管理员 package ID、固定 entrypoint 与 SHA-256，并通过 startup probe 为受验证的 runtime/package 产生 opaque proof。但 MCP 用户配置仍只有自由形式 command/args/env/cwd，尚无可信字段可将某个 server profile 对应到 catalog package。单纯把 package ID 放入 command 会让它进入宿主进程启动路径。

## 决定

- `McpServerConfig.stdio_execution` 是 MCP stdio 执行目标的唯一配置字段，采用 tagged union：`host_process`，或 `managed_package { package_id }`。旧配置缺省为 `host_process`；package ID 只允许 catalog 同一语法：1–64 字节、小写字母开头，其余为小写字母、数字或连字符。
- `managed_package` 仅允许 stdio transport，并禁止 command、args、env、env_refs、cwd 与 URL。配置校验在 `ConfigService` 写入边界执行。更新到 managed profile 时，Tools 同步清空旧 host 进程字段及凭据引用，撤销旧 client generation，并保存为 disconnected profile。
- `McpStdioLaunchSpec` 携带 typed execution profile。当前 `LocalMcpStdioProcessLauncher` 遇到 managed package 会在调用任何 host spawn API 前返回拒绝；package ID 不会被解释为命令。后续 HCS launcher 必须按 ID 从 pinned catalog 解析 profile，并使用 ADR 1274/1275 的 runtime/package proof；不得信任配置里的路径、entrypoint 或 digest。
- 授权 target binding 升至 `mcp-config-v2` 并包含 execution profile/package ID；`McpClient::matches_config` 同样比较该字段，防止旧 generation 被复用。MCP server snapshot 与 IPC 将 execution profile 显式传给 renderer。UI 只允许 stdio 下选择 managed package，并在尚未接通 broker 时明确显示 profile 会保持断开。
- 本决策不启动 HCS guest，不暴露环境变量或 workspace，也不改变 MCP transport、工具 exposure、授权或网络策略。双向字节流、背压/取消、generation 撤权和有界关闭仍是后续步骤。
- 没有数据库 schema 变化。既有 TOML 配置通过默认值继续使用 host process，无需重置；受管 profile 以 package ID 作为唯一 stdio 执行目标，host command/args/env/cwd/URL 保持为空，其余通用 MCP 字段沿用现有配置语义。

## 替代方案

- 将 package ID 填入 `command`：拒绝，因为会进入本机进程启动流程，也无法表达 catalog 身份绑定。
- 将 `command` 与 package ID 同时保留：拒绝，因为 host 命令会形成第二个执行目标并可能携带宿主路径或凭据。
- 配置保存后尝试本机连接，依赖启动失败来保护：拒绝，因为安全边界应在 launcher 的 spawn 之前显式拒绝，而不是尝试执行任意配置值。
- 在本决策中直接接通 HCS session：延后。当前仍需实现双向字节 I/O、背压、取消、generation 生命周期与有界关闭，package proof 也尚未被 session owner 消费。

## 影响与回滚

跨 Common、MCP、Tools、App、UI 与 IPC 的配置契约增加 execution profile。旧配置默认行为不变；新 managed profile 在现有版本中只可保存和展示，不能连接。回滚代码前需移除 managed profile 创建 UI/API 并清理已写入 TOML 的 `stdio_execution` package 记录，或恢复支持该 tagged field 的版本；没有数据库重置要求。移除本机 launcher 拒绝前，必须先完成 broker proof 校验和完整双向 session 生命周期。

## 验证

- Common 测试覆盖 package ID 语法、managed profile 字段约束和旧配置默认值；MCP 测试覆盖本机 launcher 在 host spawn 前拒绝 managed package；Tools 测试覆盖 authorization target binding 对 execution profile 的区分；UI 测试覆盖保存 package ID 且不提交任何 host process 字段。
- 需要运行 Rust workspace fmt、tests、strict Clippy，UI check/tests/build、IPC 生成/漂移检查与 ADR index 检查。
- 按用户要求，此步不执行真实 HCS guest 或超额写入验收；真实 Windows 容器环境和 broker token 尚未准备。
