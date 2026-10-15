# ADR 1181：在 WorkspaceWrite 下拒绝 opaque capability

## 状态

已采纳并实施。

## 背景

`WorkspaceWrite` 的可写范围只约束 Tools 自有 Files 能力，不能限制 Shell、Skill 或不透明 MCP adapter 的本地文件访问。安全设置 UI 已声明这类子进程会被沙箱拦截，但 `network_policy_block` 对 `NetworkPolicy::Ask + NetworkAccess::Opaque` 先返回允许继续确认，绕过了随后针对 `WorkspaceWrite` 的保护。一次性用户确认不能给子进程增加工作区级文件系统约束。

## 决定

- 在网络策略分支前先检查沙箱边界：`SandboxMode::WorkspaceWrite` 下 `NetworkAccess::Opaque` 一律 fail closed。该判断早于永久/会话 Allow、确认 receipt 和实际 handler。
- 此拒绝不受 `NetworkPolicy::{Ask, Open, Restricted}` 影响。选择 `WorkspaceWrite` 表示只允许受 Tools capability 约束的本地变更；如需运行 opaque adapter，用户必须先显式切到 `FullAccess`。
- 在 `FullAccess + Ask` 下，opaque capability 仍走已有确认路径；`FullAccess + Restricted` 仍因目的地不可验证而拒绝；`Deny` 保持硬拒绝。
- 授权检查和确认 receipt 重验复用同一边界函数。策略 revision 改变也会使之前的 proof/receipt 失效；此变更不把 cwd、Job Object 或环境白名单描述为 OS sandbox。

## 替代方案

- 保留 Ask 例外并依赖用户逐次确认：拒绝，确认不会限制随后运行的子进程对工作区外文件的读写。
- 只阻止 Shell 而允许 Skill/MCP：拒绝，工具名称不能替代 operation capability 的权限分类，且多个 opaque adapter 共用同一安全缺口。
- 通过解析 MCP/Shell 参数推断文件范围：拒绝，参数无法完整描述任意子进程和远端 capability 的实际访问。

## 影响与迁移

在 `WorkspaceWrite` + `Ask` 下，以前可在确认后执行的 opaque capability 现在会被拦截；用户需切换 `FullAccess`，再按 `Ask` 确认联网能力。已持久化的 sandbox/network 设置不变，不涉及配置字段或数据库迁移。该策略只限制 Haven 的授权入口；已经运行的进程仍遵循其既有 OS 权限，Haven 当前没有提供进程级 Windows 文件/网络沙箱。

## 验证

- Tools 测试覆盖 WorkspaceWrite 对 `Ask/Open/Restricted` 的 opaque 拒绝，并确认 `FullAccess + Ask/Open` 仍需确认。
- 运行 Tools 测试、workspace 测试、workspace 严格 Clippy、格式检查和 `git diff --check`。
