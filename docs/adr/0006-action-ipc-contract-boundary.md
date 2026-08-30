# ADR 0006：任务 IPC 契约边界

日期：2026-08-26
状态：已采纳

## 背景

后台任务和定时任务共用 `action:*` 事件，但此前由 `haven-tools` 直接发送临时 JSON：后台记录使用
`action_id`，定时记录使用 `id`，前端 store 需要猜测类型并在路由中读取 snake_case 字段。命令也
返回 `serde_json::Value`。这既没有稳定 DTO，也会把动态工具参数、续接 prompt 与本地日志路径扩散到
Tauri 边界。

## 决定

- 在 app shell 定义统一 `ActionEvent { id, kind, ... }`，并作为任务命令与
  `action:created`、`action:updated`、`action:output`、`action:finished` 的唯一公开载荷。
- 工具 crate 可以保留执行所需的动态 JSON，但 app shell 是唯一投影与 emit 点；不合格内部载荷记录
  warning 并拒绝发送。
- 前端以 `contracts/action.ts` 和 `actionEventListeners` 作为唯一 snake_case → camelCase 转换点；
  routes 与 stores 只消费 `ActionPayload`。
- DTO 仅包含任务面板需要的显示字段；排除 `tool_args`、`prompt`、`tool_name` 与 `log_path`。不保留
  `action_id` 字段或前端猜测逻辑。

## 替代方案

继续把工具 JSON 直接 emit，或让工具 crate 依赖 app DTO。前者无法防止内部字段成为 API；后者会让
底层 tools 反向依赖 Tauri 宿主。两者均被拒绝。

## 影响

任务事件的公开 ID 字段从后台专用的 `action_id` 统一为 `id`；前端内部字段改为 camelCase。测试版不
承诺旧 WebView listener 兼容。数据库 schema、定时任务执行和 action ID 格式不变。

## 验证

Rust 序列化测试固定 background/scheduled DTO 并确认敏感内部字段被排除；Vitest 固定 camelCase
映射与 action store 归并。完整验证执行 Rust workspace 测试、严格 Clippy、Svelte check、Vitest 与生产构建。

## 回滚与重置

可整体回滚到旧 JSON 边界，但不能在同一版本并存两种事件字段。该修改不影响用户数据库、配置或缓存，
无需重置。
