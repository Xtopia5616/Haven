# ADR 1064：统一命令与前端错误报告所有权

## 状态

已接受（2026-10-12）

## 背景

Rust command 中有少量缺失 API key 或 session 的错误绕过 `log_err`，与同一 command 的其他失败路径不一致。前端 `invoke` rejection 已经经 `logError` 记录，组件 catch 再调用 `reportError` 时仍会重复记录和镜像上报。原标记通过给错误对象定义属性实现，冻结对象不能标记；Tauri 也可能以 string primitive 拒绝，无法作为 WeakSet identity 使用。

## 决定

- command 已知失败使用命令上下文的 `log_err`。
- 前端使用 `WeakSet` 跟踪已记录的错误对象，`reportError` 对同一 invoke rejection 继续显示通知但不再重复上报；本地 parser/validation 错误仍由 `reportError` 完成记录。
- Tauri 的未分类 primitive rejection 在 invoke 边界包装为 `Error`，先记录并将同一对象抛给上层，以便页面 catch 可靠识别。
- 授权等待和过期确认保持现有控制流处理，不改变其用户可见行为。

## 影响

Rust command 与前端 invoke/页面 catch 对单次失败采用一致记录归属；冻结错误对象及字符串 rejection 不再导致重复记录。错误正文仍不写入 frontend failure kind 日志。

## 验证

- UI 回归测试覆盖冻结 Error 去重、primitive rejection 归一化以及原有本地错误报告行为。
- App 单测覆盖 resume 的精确 not-found 错误；workspace 检查、clippy、全量测试和 UI 全量测试通过。
