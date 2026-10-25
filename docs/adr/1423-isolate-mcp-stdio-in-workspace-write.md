# ADR 1423：在 WorkspaceWrite 下隔离 MCP stdio server

## 状态

已实现策略编译、调用链接入与授权回归。MCP crate 120 项测试通过；Tools 的 MCP 回归覆盖入口分流、工作区、scratch、网络边界和旧授权撤销。真实 MCP server/MXC 端到端验收尚未完成。

## 背景

MCP stdio server 是用户配置的长期运行进程。此前 process-start permit 只允许 FullAccess 且网络 Open，server 由宿主权限启动并由 `ProcessContainment` 管理；每次 MCP tool call 则另有连接目标和完整参数授权。Job Object 只能管理生命周期，不能给 WorkspaceWrite 提供文件/网络隔离。

## 决定

- process-start request 携带配置 cwd 和从准备好的命令环境发现的运行时只读目录。Platform 持有唯一的 Windows runtime dependency discovery helper；Tools 仍是目录与网络策略的唯一编译 owner。
- FullAccess 保留宿主进程/Job Object 路径，并要求 global、`mcp_stdio` 与 `mcp` 网络策略均为可执行的 Open 且没有域名约束；ReadOnly 和非 Windows WorkspaceWrite 拒绝启动。
- WorkspaceWrite 的 Windows server generation 经 MXC 启动。cwd 必须位于活动工作区写根内，但在进程中只读；命令/运行时依赖只读；每个 generation 的 app-owned scratch 是唯一额外写根，作为 `TEMP`、`TMP`、`TMPDIR` 并随 generation 生命周期回收。
- MXC 允许公网出站的条件是 global Open 且 global/MCP process 网络没有域名 allowlist；此时仍拒绝私网双向和宿主 loopback。Restricted、Deny 或存在域名限制时，WorkspaceWrite server 可离线启动，但网络标志全关。MXC 不模拟域名规则。
- MXC 失败不得回退宿主进程。Windows command wrapper fallback 只在 MXC 内尝试；不改变 permit-owner 流程。
- server process-start authorization 和每次 MCP call 的 target、连接 generation、参数摘要授权保持两个独立边界。全局过程/网络边界撤销时继续断开 stdio generation；每次调用的授权不因新 process permit 被替代。
- `load_mcp` 与 stdio 工具调用在 WorkspaceWrite 下声明 `SandboxedHost` 和无进程内网络请求；具体公网出口由该 server generation 的 MXC policy 控制。Manager 与 Client 不再提前把 Deny、Restricted 或域名范围解释为 stdio 启动拒绝，process-start authorization 是唯一启动裁决入口；HTTP transport 仍走原网络授权。
- 工作区变化以及 `mcp` / `mcp_stdio` 工具网络策略变化都会推进 process-boundary revision、通知 Manager 撤销旧 generation，并在下一次启动前刷新授权快照。替换 Manager 授权快照时同步移除旧 stdio generation，避免短暂复用旧网络权限。

## 替代方案

- 仅在 FullAccess 运行 MCP stdio：拒绝，Windows WorkspaceWrite 可以通过 MXC 将 cwd 只读、写入收敛到 scratch。
- 对任意 WorkspaceWrite MCP server 保留域名列表并允许公网：拒绝，MXC 进程网络规则无法强制域名范围。
- 授予整个工作区可写：拒绝，server 的工作目录只读，scratch 单独可写。
- MXC 启动失败后用宿主权限重试：拒绝，会越过用户选择的沙箱。
- 把每次 MCP tool call 授权合并进 server process permit：拒绝，连接进程启动和远程资源调用有不同生命周期及资源 identity。

## 影响与迁移

WorkspaceWrite 下 Windows stdio server 可以启动在受限环境中。需要网络且 global policy 有域名范围的 server 会离线启动并可能报告连接失败；有意访问 loopback/private service 的 server 不受支持。配置、数据库、MCP transcript 和 UI payload 不变，无需重置。

## 验证

- `cargo test --locked -p haven-mcp --lib`：120 passed。
- `cargo test --locked -p haven-tools --lib mcp`：64 passed。
- Tools 的 MCP 回归覆盖网络模式变化、旧 generation 撤销、WorkspaceWrite `load_mcp` / adapter contract、WorkspaceWrite policy 生成和 workspace generation 变化。
- 新增策略测试覆盖只读 cwd、唯一 scratch 写入、global Deny 下网络关闭和 cwd 越界拒绝。
- 尚无当前配置的外部 stdio server 可用于真实 MXC 联合验收；不能将 policy tests 当作 server 兼容或完整网络隔离的端到端证明。

## 回滚

可将 MCP stdio 在 WorkspaceWrite 下恢复为拒绝；不得删除 process-start 实时授权、策略变化时的 generation teardown 或逐次 tool-call target/参数授权。无需重置持久数据。
