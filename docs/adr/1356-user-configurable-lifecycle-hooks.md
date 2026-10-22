# ADR 1356：支持用户可配置的生命周期 Hooks

## 状态

已采纳并实现。

## 背景

Common 中的 `OnceHandler` 是仅安装一次的内部回调槽，ReAct 内部 Hooks 也是编排实现细节。项目没有用户或项目级的生命周期命令注册、匹配与策略决定能力。用户选择通过外部命令脚本实现首版。

## 决定

- 在 `config.toml` 的 `[hooks]` 下配置用户级命令；在工作区 `.haven/hooks.toml` 的 `[hooks]` 下配置项目命令。每个 Hook 由 `command`、字面 `args` 和可选的精确工具名/`*` matcher 组成，不经 shell 解析。命令配置和受信任路径属于 Common config-only 字段，不进入 Settings IPC。
- 项目 Hook 只在执行工作区根路径与 `trusted_hook_workspaces` 中的用户配置路径规范化后相等时读取。通过 pinned `DirectoryCapability` 读取 `.haven/hooks.toml`，限制为 64 KiB，并拒绝重解析点、非普通文件、格式错误与超限配置。
- 提供 `PreToolUse`、`PostToolUse`、`SessionStart` 和 `PreCompact`。`PreToolUse` 位于 Haven 工具授权/确认之后、工具执行之前，只允许 veto；它的 `allow` 不替代 Haven 授权。`PostToolUse` 在工具执行结果返回后运行；`SessionStart` 在每次 ReAct run 入口运行（包括恢复/续接）；`PreCompact` 在达到压缩阈值后、调用 compactor 前运行。
- 外部命令只在 `FullAccess` 与 `NetworkPolicy::Open` 下启动，因为 Job Object 只回收进程树，不构成文件系统或网络隔离。执行前重新获取并持有 live policy permit 到子进程进入 containment；进程期间监听安全策略/边界撤销并终止整棵进程树。Hooks 不得绕过工具本身的既有授权。
- 单个事件最多合并 8 个用户 Hook 与 8 个项目 Hook；输入 JSON、每个 stdout/stderr 流上限均为 64 KiB，单事件总时间上限 5 秒。命令及参数直接启动且环境清空后只恢复最小 runtime 环境。`PreToolUse` 的非零退出、超时、取消、策略拒绝、无效响应或超限均 fail closed；三个通知型事件出错只记录失败并继续主操作。通知输出不进入 transcript。

## 替代方案

- 让 Hook 使用 `WorkspaceWrite` HCS：首版拒绝。现有 HCS profile 绑定 Shell/固定 Skill 合约；任意 Hook command 缺少可证明的 runtime、输入输出快照、review owner 与步骤身份，不能复用现有隔离 profile。
- 只用 Job Object 并在 `WorkspaceWrite` 下运行：拒绝。进程树 containment 不限制同一用户身份可访问的文件与网络。
- 让 `PreToolUse` 的 allow 跳过 Haven 授权：拒绝。Hook 是附加 veto，不是另一个授权系统。
- 让任意项目文件自动执行：拒绝。项目 Hook 必须由用户在 Haven 的 `config.toml` 中显式信任精确工作区根。

## 影响与回滚

只增加 TOML 配置字段和运行期子进程，不改数据库、事件 wire contract 或 UI；无需数据迁移或重置。已有配置缺省为空，默认行为不变。配置命令不会通过 Settings 页面编辑。回滚时删除 Common Hook 配置/契约、Tools runner、Agent port/触发点与本 ADR/用户文档，并从 `config.toml` 移除 `[hooks]` 与 `trusted_hook_workspaces`；含未知字段的回滚版本会按当前严格 TOML 契约恢复默认配置并备份解析失败文件。

## 验证

- Common 测试覆盖 project TOML 解析、命令/matcher 校验、Debug 脱敏、Settings IPC 隔离和 config round-trip。
- Tools 测试覆盖精确 matcher、PreToolUse allow/deny 响应、payload 容量、精确 workspace trust，以及默认 `WorkspaceWrite` 下 PreToolUse fail-closed。
- 完成后记录 workspace check、相关测试、严格 Clippy、格式与 ADR 索引检查结果。真实 Windows Job Object 进程回收仍需 Windows 环境验收；本地编译/测试不能替代该系统行为验收。
