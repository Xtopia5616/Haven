# ADR 0147：工具契约加固与任务/交互生命周期统一

## 状态

已接受（2026-09-14）

## 背景

Haven 的内置工具已经具备 operation view、风险矩阵、取消、并发和统一媒体资产入口，但少数边界仍会把模型可见契约与实际副作用错开：注册表删除的歧义可能扩大删除范围，router 的存在不能证明 OCR 能力，inbox 的确认时点可能早于会话投影，文件改写缺少原子提交，窗口和 UIA 目标也可能在界面变化后失效。同时后台任务、定时任务和人工等待各自维护生命周期，模型缺少统一的查询和恢复语义。

## 决策

1. 注册表删除拆为 `system.registry.delete_value` 和 `system.registry.delete_key`。值删除必须提供 `name`；键删除明确为递归删除并提升为 Critical。
2. `media.ocr` 只在专用 OCR client 存在时注册 capability。普通视觉 router 不再被当作 OCR provider；无 capability 时工具 view 不暴露 OCR operation。
3. inbox 使用 `claim → process/project → durable snapshot → ack`。显式 inbox 默认只 claim，返回进程内 `claim_token`；ack 可以使用 token 完成整批 claim，也可以用稳定 message id 选择性确认。崩溃或重启只会导致 at-least-once 重投。
4. `system.env` 明确 `process`、`user`、`machine` scope。Windows 的后两者通过持久化环境键和 `WM_SETTINGCHANGE` 实现；非 Windows 明确拒绝持久化 scope。
5. `files.write/edit/patch` 统一走同目录临时文件、flush/sync 和原子替换，并支持 `expected_hash`、`dry_run` 与写入大小上限。校验失败不提交部分结果。
6. 窗口操作使用 `window_id` 和 UIA `element_token`；`observe` 一次返回窗口身份、UI 树、截图 asset，并提供 Invoke/Value/Toggle/SelectionItem 语义操作。坐标点击保留为无稳定目标时的后备路径。
7. 增加 `ActionService` façade，将后台和定时任务规范化到同一查询、状态和取消入口；现有 worker 暂时保留为内部执行实现。增加 `InteractionRequest` 作为 ask/confirm/scheduled confirm 的共同持久化投影和状态转换模型。
8. Clipboard 支持文本、HTML、图片和文件列表；图片/文件读取复制到受管媒体目录并返回 `asset_id`。增加 `media.render`，复用有界文档表示管线按页返回文本/表示结果。

明确不增加 `http.search`、独立 `audio` 或 `file_search` 根工具：搜索继续由 provider-level web search/MCP 提供，音频和文件能力继续分别归 `media` 与 `files`。

## 取舍与后续

这轮先建立兼容的 façade 和契约边界，不在一个变更中重写后台 worker、scheduled worker 或前端 interaction store。旧 snapshot 字段仍可读取，新的 `interactions` 是规范化投影；后续可在数据库重置/事件 schema 变更窗口删除兼容字段，并把两个 action worker 收敛为真正的持久化状态机。

## 验证

- operation view、schema、prompt、风险矩阵对新增/拆分 operation 一致；
- registry 缺少值名、router-only OCR、错误 expected hash、dry-run 和 claim token 有回归覆盖；
- `cargo check --workspace --locked`、`cargo test --workspace --locked` 与 `cargo clippy --workspace --locked -- -D warnings` 通过；
- 通过 Windows 条件编译检查持久化环境变量、剪贴板 HTML/文件列表和 UIA 实现。
