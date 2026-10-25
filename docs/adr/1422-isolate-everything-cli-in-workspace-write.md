# ADR 1422：在 WorkspaceWrite 下隔离 Everything CLI

## 状态

已实现策略编译和进程入口。Everything 策略回归测试通过；MXC 与本机 Everything service IPC 的端到端验收尚未完成。

## 背景

Everything CLI (`es.exe`) 是可配置的第三方进程。之前它只受查询根校验、输出/时限上限和 Windows Job Object 约束；Job Object 不限制文件或网络访问。Everything 的索引服务是独立 IPC 对端，因此限制 CLI 进程仍不能证明服务本身的可见范围。

## 决定

- FullAccess 保留宿主进程和 Job Object 行为，并沿用要求 Open 网络的入口门槛。WorkspaceWrite 只在 Windows 通过 MXC 启动，ReadOnly 和不支持 MXC 的主机拒绝。
- WorkspaceWrite 只接受工作区内的目录作为搜索根。选定目录、命令所在目录和所需运行时目录只读；单次 app-owned scratch 可写；网络完全关闭。文件根、工作区外目录、deny/read-only 冲突和策略根无法打开时 fail closed。
- MXC probe/spawn 失败不得回退宿主启动。Everything service 的 named-pipe/其它本机 IPC 是否可用由真实运行时单独验收；工具仍对 service 返回的每个路径执行现有根、deny 和 Files read policy 校验，服务索引不作为授权来源。
- 搜索结果上限、stdout/stderr 预算、15 秒进程期限、取消和进程清理保持原语义。仅策略与路径范围改变，没有配置、数据库或 UI wire contract 迁移。

## 替代方案

- 仅用 Job Object：拒绝，无法限制子进程文件/网络访问。
- 在 MXC 中授予整个工作区：拒绝，搜索只需要选定目录只读。
- 允许 CLI 使用公网上传或访问本机服务：拒绝；WorkspaceWrite 当前目标不要求网络，服务 IPC 是未证明的外部执行边界。
- MXC 失败后回退宿主进程：拒绝，会静默扩大权限。

## 影响与迁移

WorkspaceWrite 下 Windows Everything 搜索可能因 MXC 后端、配置的根范围或 Everything service IPC 限制而失败。逐条结果继续经过 Files 授权过滤。FullAccess 与既有调用契约保持不变；无需重置配置或持久数据。

## 验证

- Everything CLI 的 JSON 解析、根包含、过滤和 bounded output 测试通过（5 项）。
- WorkspaceWrite policy regression 验证只写 scratch、只读搜索根、关闭公网/私网/宿主 loopback，并拒绝工作区外根。
- 当前没有可用于稳定复现的真实 Everything service 配置；MXC 与 service IPC 交互必须在启用 Everything 的 Windows profile 上单独验收。

## 回滚

可恢复 WorkspaceWrite 下拒绝 Everything CLI，但不得移除已有的搜索根与 Files policy 校验、输出预算、取消或进程树清理。无需重置持久数据。
