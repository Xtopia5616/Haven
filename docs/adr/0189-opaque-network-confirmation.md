# ADR 0189：opaque 网络能力进入普通确认流程

- 状态：Accepted
- 日期：2026-09-21
- 范围：`haven_tools`、`haven_common`、权限架构文档
- supersedes：ADR 0154 中“`ask` 不放开 opaque 子进程”的决定；保留其余网络策略语义

## 背景

Skill 和 MCP 适配器的目的地由外部脚本或 provider 决定，授权契约只能声明为
`NetworkAccess::Opaque`。默认网络策略已经是 `ask`，但授权网关仍把 opaque 能力作为
硬拒绝处理，导致模型无法创建普通确认请求；用户必须先切换到更宽松的全局安全组合。

## 决定

1. `NetworkPolicy::Ask` 对 opaque 能力不再直接返回 `Blocked`，而是继续执行授权引擎的
   普通确认判断。Skill 的高风险与敏感外部效果仍会触发确认，已有 allow/deny、receipt
   和 policy revision 规则保持有效。
2. `NetworkPolicy::Deny` 继续阻断全部网络能力；`Restricted` 继续要求 Haven 能验证目的地，
   因此仍阻断 opaque 能力。
3. `Open + WorkspaceWrite` 继续拒绝无法被 Job Object 约束文件系统/网络范围的 opaque
   子进程；`Ask` 是面向用户的显式确认例外，不表示 Job Object 具备额外隔离能力。

## 验证与回滚

授权单测覆盖默认 `ask` 下的 opaque 确认、`deny`/`restricted` 硬拒绝，以及
`open + workspace_write` 的既有负向边界。回滚只需恢复网关对 `Ask + Opaque` 的拒绝分支，
无需配置或数据库迁移。
