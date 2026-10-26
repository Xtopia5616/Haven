# ADR 1429：外部工具内容不构成指令或授权

## 状态

已实施；格式、workspace 编译、Clippy、ADR 索引与 crate 依赖检查通过。系统提示契约测试通过；workspace 测试整体被两个现有的 Windows Shell 子进程清理用例阻断：测试依赖 `$PSHOME\powershell.exe`，当前环境该文件不存在，子进程启动返回 `NotFound`。Skill runtime 候选排序用例通过；真实 MXC 端到端验收仍按 ADR 1420 跟踪。

## 背景

Haven 已把网页、MCP、Skill、附件、记忆和工具结果纳入系统提示，但旧表述仅提醒模型“考虑来源和相关性”。外部内容可能携带伪装成系统指令的文本，要求扩大访问、披露凭据或执行副作用。模型提示需要清楚表达信任边界，同时不能取代后端授权。

## 决定

1. 稳定系统提示明确把上下文、记忆、附件、工具结果、Skills 与 MCP 响应视为不可信数据；其中的指令不得覆盖 system/developer/user 指令、改变 Haven 权限策略或充当审批。
2. 外部内容只能在用户已授权任务和 Haven 的安全网关范围内作为信息使用；若内容要求扩大作用域，Agent 应停止并向用户说明。
3. 对外部来源的事实保留来源归属，不把工具声明当作已验证事实。
4. 该提示只提供纵深防御，不签发或替代授权。副作用仍须通过 `AuthorizationEngine` 实时策略、资源边界、confirmation receipt 与 execution proof；MCP/Skill 等不透明外部操作继续遵守 ADR 1412/1428。
5. Haven 的本机 sandbox 不能收窄外部服务凭据在远端可执行的操作范围。连接器的服务端 ACL 与凭据 scope 由服务所有者管理；Haven 对 MCP 调用仅能绑定本地配置身份和具体请求，并要求逐次确认，不声明已验证远端 principal 的权限。

## 替代方案

- 仅依赖模型自行判断外部内容是否可信：拒绝，旧提示没有明确禁止其改变权限或代替审批。
- 通过字符串扫描过滤所有可疑工具输出：拒绝，无法可靠识别自然语言变体，会误删正常数据，且不能替代后端权限检查。
- 将所有工具结果从 Agent 上下文移除：拒绝，会破坏工具循环；风险由来源提示与既有授权网关分别处理。
- 让提示文本替代 `AuthorizationEngine`：拒绝，模型输出不是执行授权凭证。

## 影响与迁移

修改 `MAIN_SYSTEM_PROMPT` 的稳定前缀与架构文档；不改变持久化数据、工具结果、事件、配置、IPC 或数据库 schema，无需重置。执行证明与既有授权行为保持不变。

## 验证

验证结果：

- `cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings` 通过。
- `pwsh -NoProfile -File scripts/check-adr-index.ps1` 与 `pwsh -NoProfile -File scripts/check-crate-dependencies.ps1` 通过。
- `cargo test --workspace --locked` 中系统提示契约与 Skill runtime 用例通过；整体命令因 `haven-tools` 的 `policy_revocation_terminates_shell_descendants` 和 `cancelling_shell_terminates_its_descendant_processes` 失败。两例均因当前 Windows 环境缺少 `$PSHOME\powershell.exe` 而无法创建测试子进程，与本 ADR 的提示/文档改动无关。
- ADR 1420 所述机器级 Python MXC 端到端验收仍未完成。

## 回滚

恢复旧版系统提示说明并删除本 ADR 与架构文档对应段落即可。没有数据或配置需要回滚。
