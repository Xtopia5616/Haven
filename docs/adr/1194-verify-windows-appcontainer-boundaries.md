# ADR 1194：验证 Windows AppContainer 文件与网络边界

## 状态

已采纳并实施（仅可行性测试，不代表生产执行器已接入）。

## 背景

ADR 1190 和 1193 已明确：Haven 的 `SandboxMode` 与 `NetworkPolicy` 约束工具授权网关，不会为 Shell、Skill 或 MCP stdio 子进程创建 Windows OS 级文件/网络边界。启用 AppContainer 前，需要先确认当前 Windows 进程启动与 workspace 权限模型能否表达并实际执行该边界。

AppContainer 进程使用低完整性令牌。单独给 package SID 添加目录 DACL 不足以让它写入默认中完整性目录；目标 workspace 还必须有兼容的 mandatory integrity label。这个差异必须作为执行 workspace 的安全契约处理，而不能被 `cwd` 或普通路径校验掩盖。

## 决定

- 在 `haven-platform` 增加 Windows-only 集成探针：创建唯一临时 AppContainer profile，给 package SID 仅在测试 workspace 上授予读写 DACL，并临时将该目录及新建内容设为低完整性标签；测试退出时恢复原 DACL 与标签。
- 探针以 `SECURITY_CAPABILITIES` 启动无额外 capability 的子进程，先创建为 suspended，再加入现有 Job Object，最后恢复执行。子进程必须能写入 workspace，并且必须无法读取或覆写 workspace 外哨兵文件，也不能连接本机 loopback listener。
- 该测试使用复制到获准 workspace 的当前测试二进制作为探针程序，避免把 PowerShell 或脚本运行时依赖混入 OS 边界验证。
- 本 ADR 只证明 Windows AppContainer 能表达本测试的 workspace 与网络约束。测试 helper 继承测试 runner 的环境；生产 Shell、Skill、MCP 启动器仍须接入 filtered environment、显式句柄管理、workspace 代次/授权快照、取消与进程树生命周期，并为各 adapter 增加端到端拒绝测试。
- Haven 现有 `WorkspaceWrite` 对 opaque 子进程的拒绝保持不变；`FullAccess` 下的生产子进程也仍使用当前 Windows 用户的文件和网络权限。不能将这个 test-only probe 描述成已交付的产品 sandbox。

## 替代方案

- 只测 package SID DACL：拒绝作为充分验收；低完整性令牌还受 Mandatory Integrity Control 约束，默认中完整性 workspace 不可写。
- 使用 PowerShell 作为探针：拒绝；PowerShell 启动/模块环境会把运行时兼容性与 AppContainer 边界混在同一结果中。
- 直接在本切片接入所有 Shell、Skill、MCP adapter：拒绝；需要先设计可承载 AppContainer 的进程启动器、过滤环境、标准流/继承句柄和现有 Job Object 的统一接口，再逐个迁移调用方。
- 以 Job Object、环境清理或 `cwd` 代替 OS sandbox：拒绝；这些能力不限制子进程的文件 ACL 或 socket 权限。

## 影响与迁移

仅增加 Windows test-only FFI 探针和 `haven-platform` 所需 Windows API feature；不改变应用行为、配置、IPC、数据库或持久数据，无需重置。测试显式创建临时 profile 与文件，并在退出路径恢复 workspace 的原始权限元数据。

## 验证

- Windows 上运行 `cargo test --locked -p haven-platform appcontainer_process_is_limited_to_granted_workspace_and_has_no_network_capability -- --nocapture`，验证 workspace 写入成功、外部文件读写被拒绝、loopback 连接未到达 listener。
- 运行 `haven-platform` 的完整 locked 测试、严格 Clippy 与格式检查，以及 ADR 索引、依赖边界和 `git diff --check`。
- 生产 adapter 尚未迁移；它们的 filtered-environment 与完整调用链 OS 边界验收仍为路线图未完成项。

## 回滚

删除本 ADR、路线图与 ADR 索引中的引用，移除 Windows-only 探针模块及其 `windows-sys` feature；不需要恢复配置或持久数据。
