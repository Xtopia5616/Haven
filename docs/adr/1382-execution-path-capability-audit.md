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

### Windows 部分验收（2026-10-24；Acceptance Open）

在 Windows 11 Insider 29683、Rust 1.99.0、Node 24.21.0、pnpm 12.12.1 上，以 `527bcd3336cc6d6526c3ce6a172f778918b0dbae` 为基线，通过 `cargo tauri dev --config crates/app-binary/tauri.dev.conf.json` 在独立 `HAVEN_DATA_DIR` 启动开发版。AppState、Tauri 初始化和 bootstrap ready 均完成；数据库 schema v61，SQLite integrity check 返回 `ok`。未配置模型 endpoint，未使用真实 Haven 数据或凭据。

真实 Windows/MXC 负向测试命令 `cargo test -p haven-tools --locked workspace_write_shell_is_confined_to_configured_roots_by_mxc -- --ignored --nocapture --test-threads=1` 通过（1 passed）。该测试走生产 Shell/MXC 路径，配置根内写入成功，兄弟目录写入被拒绝。

首次 GUI 验收曾被 Node.js Windows 防火墙权限提示阻挡，提示由用户手动处理，自动化未更改防火墙规则。2026-10-24 续测时旧开发进程已经退出；主 checkout 为 `6f60ef1`，与 `527bcd3` 只有三份验收文档差异，运行代码相同。沿用原隔离 profile 重新执行上述 `cargo tauri dev` 命令后，真实窗口显示首屏、Settings 和安全与权限页。安全页明确展示“协作执行”“工作区范围”“受限网络”，并说明已开始调用保留旧工作区、子进程隔离由具体执行边界决定；本次只读核对，未修改安全设置。

完整授权/确认、网络策略、子进程后代、取消/撤权及 backend 不可用时无 host fallback 的 UI/平台场景尚未验收；未配置模型 endpoint，也未通过 UI 发起 Shell。当前 Windows Computer Use 执行限制禁止自动修改应用内安全/隐私设置或通过 UI 运行终端命令，因此不以界面说明代替实际授权/执行证明。Slice 3 保持 Acceptance Open。

2026-10-24 在当前提交 `a2b1720c463796a0bb1f81c4e5c4ecd7324fb8b3` 上，以 Rust 1.99.0、Node 24.21.0、pnpm 12.12.1 和隔离 `HAVEN_DATA_DIR` 再次启动开发版；AppState、Tauri 初始化和 bootstrap ready 完成，schema v61，SQLite integrity check 为 `ok`，未配置模型 endpoint 或使用真实数据/凭据。Windows 当前环境报告 Windows 10 Pro、build 29683，尚未据此确认满足 Windows 11 24H2 支持基线。首屏被 Windows Defender Firewall 的 Node.js 网络权限提示遮挡；按 computer-use skill 的规定未自动选择允许或取消，也未修改防火墙规则。此轮没有进一步交互，因此不计入 UI 验收；待用户处理提示后才能继续当前构建的只读界面核验。
