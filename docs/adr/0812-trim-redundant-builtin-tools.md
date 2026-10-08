# ADR 0812：精简冗余内置工具

## 状态

已接受（2026-10-08）

## 背景

模型可见工具中有几项只在当前进程内保存临时状态，或只是已有工具的窄包装，增加了目录、授权规则和 UI 结果渲染的维护成本。进程查询/结束可由受控 shell 命令完成；环境变量和注册表修改也可通过同一高风险 shell 边界显式执行。窗口 OCR 另有统一的受管媒体资产流程。

## 决定

- 删除 `checklist.*` 与 `preferences.*`。二者的数据都只存在于进程内并按 session 隔离，不能在重启后恢复；模型可在对话中维护临时计划和偏好。
- 删除 `process.list` 与 `process.kill`。需要进程信息或结束进程时使用 shell，并继续受 shell 授权与安全策略约束。
- 删除 `system.env.set`、`system.env.unset` 和注册表写入/删除 operation。保留环境变量和注册表读取；修改系统配置时使用受控 shell，并经过 shell 的高风险授权。
- 删除 `window.ocr` 及 `window.observe` 的 OCR 快捷参数。窗口截图登记为受管 asset 后，使用统一的 `media.ocr`。

## 影响与取舍

- 模型目录、操作策略元数据、系统提示、工具安全回归矩阵及 UI renderer 不再暴露这些 operation。
- 已保存但指向已删除 operation 的 ToolConfig 或授权项不会获得执行入口，也不需要数据库迁移；普通配置无需重建。历史 transcript 中的旧工具调用仍按历史内容保留，不自动转换。
- 进程操作和系统设置修改会经过通用 shell 边界，失去旧 operation 的专用参数和结果卡片；Shell 权限仍独立生效。
- 窗口 OCR 与其它媒体 OCR 共用 asset 生命周期、provider capability 和授权策略。

## 验证

- 工具目录测试断言已删除的 operation 不再注册。
- Rust 与 UI 检查覆盖工具注册、schema、授权元数据、提示词和结果渲染的一致性。
