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
- 对 0.1.2 acceptance 包启动的日志显示 AppState、Tauri 初始化和 bootstrap ready 完成；当前隔离模型 endpoint `127.0.0.1:34197` 未运行，因此没有在此轮从当前包完成模型交互、确认或 Schedule/后台任务流程。
- Computer Use 经 `node_repl` 与 `@oai/sky` 可初始化。首次选中的 Haven window id `21236804` 在 `get_window` 时已经失效；刷新后当前窗口列表返回两个同名 Haven 候选 `135420` 与 `4984338`。按 Computer Use 流程未猜选窗口、未执行点击或输入；当前包的可见 UI 流程仍未验收。
- 尚未在当前包执行 durable UI 通知丢失补齐或 rollback 与恢复并发的真实 WebView 操作。
- 系统 C 盘约剩余 7.4 GiB、D 盘约剩余 7.1 GiB，无已配置的 VM，当前执行 token 无管理员提升。没有填满宿主盘，也没有创建挂载 VHD；物理 ENOSPC 的分类、用户提示和恢复操作未验收。

继续验收需要一个稳定的 Haven UI 窗口、隔离模型 fixture（确认/Schedule 相关步骤）、以及可安全耗尽的 Windows 测试盘或 VM。不得填满用户的宿主系统盘或数据盘来替代隔离测试。
