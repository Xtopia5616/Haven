# ADR 1387：为 Settings 失败提供可操作说明与安全诊断

## 状态

已实施 — 2026-10-24。

## 背景

Settings 保存凭据、应用已保存配置或在离开确认中重试时，底层错误可能来自凭据归属校验、Windows 凭据存储或部分运行时 apply。只展示原始错误不便于用户采取下一步操作；将这些失败转换成页面文案时，也必须保留统一的错误格式化、脱敏和通知路径。

## 决定

1. `presentSettingsFailure` 为已知凭据与运行时 apply 失败提供中文标题和修复动作；未知失败显示通用说明。部分 apply 明确表示配置已保存，但某些功能尚未生效。
2. Settings 错误保留在页面上下文中，`SettingsErrorNotice` 默认折叠技术诊断。诊断仅取自 `formatError` 的限长、换行规整和脱敏结果，不增加原始错误日志或 IPC 字段。
3. 页面 catch 仍通过 `reportError` 统一记录和通知。`reportError` 可按失败状态选择 error 或 warning toast，避免页面自行拼接第二条通知路径。
4. 离开确认复用同一错误说明并把保存动作标为重试；重新应用失败时状态区保留错误说明与再次重试入口。

## 替代方案

- 直接展示原始错误：说明对用户不够明确，也容易把实现细节放在主要交互中。
- 只显示通用 toast：离开确认或返回 Settings 后无法继续查看失败原因和处理建议。
- 页面自行调用 `addNotification`：会绕过 `docs/conventions.md` 规定的异常报告入口。

## 影响与兼容性

仅改变前端错误展示和 `reportError` 的 toast 严重级别选项；不改变 Settings 配置持久化、凭据存储、运行时 apply、Tauri IPC 或数据库 schema。错误诊断仍由 `formatError` 处理，无需配置迁移或数据库重置。移除新 UI 并还原 Settings 错误呈现即可回滚。

## 验证

- `corepack pnpm run check`：通过，0 errors、0 warnings。
- `corepack pnpm run test:run`：通过，168 个测试文件、1446 项测试。
- `corepack pnpm exec prettier --check`：所有本轮 UI 源码通过格式检查。
- `scripts/check-adr-index.ps1`：通过，ADR 编号唯一且本地链接有效。
- 未运行生产构建；本切片不改路由、构建配置或生产资源。
