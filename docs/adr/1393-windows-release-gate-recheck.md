# ADR 1393：Windows 发布 Gate 部分复验与开放项

## 状态

部分验收通过，Windows 发布 Gate 保持 Open。

## 背景

ADR 0395 与路线图 §5.1 要求对当前 Windows 安装包验收真实交互生命周期、durable UI 恢复、rollback 竞态、安装升级卸载和物理磁盘耗尽。此前有 0.1.1 包的 UI/模型/附件 smoke 结果，但 NSIS 资源残留修复后需要在当前代码提交和 0.1.2 acceptance 包上复验适用门禁并更新未完成项。

## 决定

- 只签核已有直接证据的内容：当前代码完整 Rust/UI 质量门禁通过；0.1.2 acceptance 包的安装、升级、运行中升级、卸载和数据保留按 ADR 1392 通过。
- 不以静态测试、旧版本交互结果或启动日志替代未完成的真实 UI/物理盘验收。Gate 保持 Open，直到路线图 §5.1 列出的每项均有当前包和隔离环境中的结果。

## 验证与限制

2026-10-24，主机为 Windows 11 专业版 Insider Preview `10.0.29683`，数据库 schema v61，工具链 Rust 1.99.0、Node 24.21.0、pnpm 12.12.1。当前代码提交为 `09123d9`；0.1.2 是通过未跟踪的 Tauri config version override 构建的 acceptance 包，仓库配置版本仍为 0.1.0。安装包身份、哈希及安装生命周期结果见 [ADR 1392](1392-nsis-cleanup-content-hashed-resources.md)。

- `cargo test --workspace --locked --quiet`：workspace 全部通过，忽略项按项目标记保留。
- `cargo clippy --workspace --locked -- -D warnings`：通过。
- `corepack pnpm run check`：0 errors、0 warnings。
- `corepack pnpm run test:run`：169 个文件、1493 项通过。
- 当前从 0.1.2 acceptance 安装目录启动的进程使用隔离 `target/acceptance-release-20261024` profile；`@oai/sky` 返回且只返回一个 Haven 窗口。设置页正常加载：对话/行为默认值、模型配置和媒体配置均可见，未修改配置。Provider `Haven Acceptance Local` 指向 `http://127.0.0.1:34197/v1`，API Key 只确认“已配置”，不读取或记录其值；目录为“未加载”，应用状态为“模型不可用”。端口检查确认 fixture 当前未监听。
- 对话、内置工具、历史和任务页面可见加载：对话初始为空；选择示例只填入草稿，再清空草稿成功，没有发送消息或持久化会话。内置工具目录加载 124 项操作。历史页显示 0 条会话；任务页显示 0 个后台任务、0 个定时计划。
- 媒体设置页可见加载，包含附件策略和当前媒体限制，未产生未保存设置。附件按钮打开了隔离验收目录的系统文件选择器，并显示 `release-attachment.txt`（74 字节）。选择附件时，Computer Use 在按要求激活 Haven 并刷新后重试一次，仍拒绝输入：`point (1147, 454) is over msedgewebview2.exe "", not target window haven-app-binary.exe "Haven"`。未打开文件、未附加或发送；选择器当时仍等待人工关闭，因此媒体附件完整流程未通过。
- 当前隔离数据库只读 `PRAGMA quick_check` 返回 `ok`，sessions/messages/session_events 数量均为 0；本轮没有发现数据库损坏。此前 ADR 1392 的 1 个 session/8 条 message 是安装卸载验收时的读数，不能替代当前 profile 的非空恢复验收。
- 因 fixture 未监听，没有从当前包运行模型响应、工具确认通过路径或 Schedule/后台任务。durable UI 通知丢失补齐、rollback 与恢复并发仍未做真实 WebView 操作。
- 当前 C 盘约余 7.23 GiB、D 盘约余 7.26 GiB。没有已配置 VM、可丢弃的挂载测试卷或管理员提升；没有填满宿主盘。物理 ENOSPC 的分类、用户提示和恢复操作仍未验收。VM 不是唯一办法：隔离的专用 VHD/测试卷也可验证应用看到的卷满错误；宿主 C/D 盘不能用来做耗尽测试。

继续验收需要用户手动关闭仍打开的系统文件选择器、启动隔离模型 fixture（确认/Schedule 相关步骤），并提供一个可安全耗尽的独立测试卷或 VHD。不得填满用户的宿主系统盘或数据盘来替代隔离测试。
