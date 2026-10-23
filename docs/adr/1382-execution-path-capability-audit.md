# ADR 1382：盘点 Shell 以外的进程执行路径并统一 capability 报告

## 状态

已实施 — 2026-10-23。Slice 3 的静态 capability 盘点见[执行路径 capability 报告](../execution-path-capability-audit-2026-10-23.md)。

## 背景

ADR 1368 为 WorkspaceWrite Shell 建立了 MXC ProcessContainer 路径。仓库还通过 MCP stdio、Skill、LSP、Lifecycle Hook、Everything CLI、后台 Shell 与 App 系统集成启动其它子进程；仅检查 `shell` 会高估文件/网络隔离覆盖范围。`ProcessContainment` 管理 Windows Job Object 进程树与生命周期，不是资源 sandbox。当前动态 `ToolCapabilitySnapshot` 只报告媒体和 Web Search，也没有统一展示进程路径能力的契约。

## 决定

1. 将 Shell 前台/后台、MCP stdio、Skill setup/runtime、LSP、Lifecycle Hook、Everything CLI 及 App/Windows helper 列为独立进程入口；HTTP MCP 和 Haven 进程内 task 标为非本机子进程路径。
2. capability 报告分列操作授权、进程树控制、真实文件/网络隔离与动态能力摘要。Job Object 不能作为文件/网络隔离的证据。
3. 当前只有前台 WorkspaceWrite Shell 用 MXC ProcessContainer 落实资源边界；MXC probe 不接受完整策略时拒绝 spawn，不回退为 host process。其它入口仅记录其本身的授权、路径/环境约束、取消和 Job Object 能力，不推断其具有 OS 文件/网络 sandbox。
4. 本 ADR 新增静态审计文档，不宣称新增动态 capability API 或改变运行行为。若后续需要运行时进程 capability snapshot，应由 Platform 提供窄 snapshot，再由 Tools 按每个入口的实际请求验证 backend 支持面；消费者不得持有通用 platform manager。
5. 后台 ToolRun/Schedule 的独立工作绑定和配置代次连接仍按路线图后续建议处理，不由本切片隐式补入。

## 影响与兼容性

无数据库、配置、IPC 或执行行为变化；无需重置。MXC 的真实 Windows 行为仍属于 Windows 发布验收 Gate。

## 验收与限制

静态审核 `Command` / `tokio::process` / Win32 spawn 使用点及对应调用链，核对 spawn authorization、`ProcessContainment`、MXC、资源策略和调用触发方式。该审核没有替代 Windows Job Object/MXC 的运行时负向验收，也没有测试每个第三方子进程的 OS 访问行为。
