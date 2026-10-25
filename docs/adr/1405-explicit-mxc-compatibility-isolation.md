# ADR 1405：将 MXC 兼容隔离改为显式选择

## 状态

已采纳并实施。

## 背景

ADR 1391 为 WorkspaceWrite Shell 增加了 MXC AppContainer 兼容 tier。BaseContainer 的 probe 或 spawn 失败后，Haven 会自动重试兼容 tier，并可能进一步把网络策略收窄。AppContainer+DACL 需要临时调整获准目录 DACL，再由 MXC 在进程结束时恢复。自动兼容使同一份配置在不同主机上产生不同隔离层、网络能力和 ACL 行为。

## 决定

- `SecurityConfig.mxc_isolation_mode` 是 WorkspaceWrite Shell 的唯一兼容策略开关，值为 `strict` 或 `compatibility`；省略字段的旧配置按 `strict` 读取，不要求配置重置或数据迁移。
- 默认 `strict` 只接受 MXC probe 明确报告、无需 DACL 补强的 BaseContainer。probe、策略构造或 spawn 失败会拒绝本次 Shell，不重试 AppContainer、不自动收窄网络，也不回退宿主进程。
- 用户显式选 `compatibility` 后，Haven 先尝试原始请求的 BaseContainer；失败时才按既有顺序尝试兼容 MXC tier，并在必要时将网络由原请求收窄为允许出站、拒绝入站与 host loopback，最后再尝试完全断网。原始网络授权为禁止时始终保持全断网。
- 文件策略仍由授权 owner 编译。明确 deny 路径、重叠只读/可写策略等要求 MXC BaseContainer 原生能力时，即使启用兼容策略也拒绝无法完整执行该文件策略的兼容 tier。
- 每次成功的 WorkspaceWrite Shell 结果都包含 `sandbox_notice`，说明实际隔离 tier、出站/入站/本机回环网络能力和 DACL 是否被临时调整及何时恢复。兼容失败原因、probe warnings 和主机准备建议继续随结果反馈。
- 切换该设置会更新授权策略版本并撤销受该设置管理的进程，使后续执行读取新策略。

## 替代方案

- 继续自动兼容并只显示降级提示：拒绝，用户无法在执行前选择 DACL 与网络语义。
- 删除兼容 tier：拒绝，用户仍可为旧主机显式选择兼容行为。
- 兼容失败后使用宿主权限启动：拒绝，这会丢失 WorkspaceWrite 的 OS 文件/网络边界。

## 影响与回滚

现有配置缺少新字段时默认严格，符合默认安全策略；配置文件仍可被正常读取，不需要清理数据库或缓存。BaseContainer 不可用的主机在默认设置下会拒绝 WorkspaceWrite Shell，用户可在安全设置中显式选择兼容隔离。兼容模式仍可能在 AppContainer+DACL tier 临时调整授权目录 DACL，并可能开放出站网络；结果会明确显示本次实际行为。

回滚需同时移除设置字段、Settings 控件、授权透传与运行时严格分支，并决定是否恢复 ADR 1391 的自动兼容默认；不得静默恢复自动降级。

## 验证

- 配置解析与序列化验证旧配置默认严格，显式兼容值可往返。
- MXC 单元测试验证严格模式的 AppContainer 拒绝和 probe/spawn 失败不重试；兼容模式验证原有出站到离线的降级顺序，并验证每次成功结果的实际 tier、网络与 ACL 说明。
- UI 测试验证可显式选择兼容隔离并显示 DACL 行为说明。
- Windows 真机仍需覆盖 BaseContainer 与 AppContainer+DACL 两条路径；静态单元测试不能代替真实 ACL 恢复验收。
