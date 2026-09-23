# ADR 0215：任务面板 hydration 使用 typed ActionView

- 状态：accepted
- 日期：2026-09-23
- 范围：`haven-tools`、`haven-app-binary` 的任务面板 hydration
- 关联：[架构降复杂度重构路线图阶段 3](../architecture-refactor-roadmap.md#阶段-3存储-domain-ports-与-typed-projection)、[ADR 0006](0006-action-ipc-contract-boundary.md)

## 背景

`ActionService::board()` 曾返回 `Vec<serde_json::Value>`，app 的 `list_actions` 再根据 JSON `kind` 分别调用后台/定时任务解析器构造 `ActionEvent`。同一条任务面板投影因此先作为动态 JSON 生成，再被反解析为稳定 DTO；内部字段也需要依赖解析器逐字段过滤。

## 决定

1. `haven-tools` 定义最小 typed `ActionView` 和 `ActionViewKind`，board 返回 `Vec<ActionView>`。状态使用 `haven_common::ActionStatus`。
2. `ActionView` 只携带任务面板字段：身份、kind/status、会话与生命周期时间、定时任务展示信息、后台命令/输出/错误/退出码和预览。shell 元数据、日志路径、动态工具参数、工具名、续接 prompt 与依赖 watch id 不进入 DTO。
3. app 边界通过明确的 `From<ActionView> for ActionEvent` 转换。`ActionEvent` 的字段、可选字段省略规则及序列化值保持不变；历史查询仍从持久化记录直接构造同一 wire DTO。
4. board 成员和排序保持现状：内存中的后台任务（包括 TTL 内终态）以及 live 定时任务；按 `started_at` 升序，缺失时间排在有时间的行之前。后台输出预览仍最多 200 个字符。
5. 实时 `event_bridge` 继续从动态生命周期 payload 投影为 `ActionEvent`，因为该生产路径仍接收事件 Value；本决定只替换 board hydration 的 JSON 往返。
6. 持久化行统一由 `ActionService::list_persisted_actions(kind)` 经其已绑定的数据库读取。app 命令不得绕过 service 直接查询 `AppState.db`；未绑定数据库时返回明确错误，不能伪装成空历史。`list_actions` 继续合并 live board 与持久化后台行、按 ID 去重，预览最多 200 字符；`list_action_history` 继续只保留终态行、过滤后再限量，默认 50 条且最多 200 条。上述投影继续由 app 命令负责。

## 替代方案

- 继续用 `Value` 并复用实时事件解析器：保留两套 JSON 形状和按 kind 分派，也无法在 tools/app 边界表达固定字段集合。
- 让 `ActionView` 直接成为 Tauri wire DTO：会把 app IPC 所有权移入 tools crate；不采用。
- 把 MCP 或调度参数也纳入 DTO：面板不需要这些执行字段，且可能包含敏感内容；不采用。

## 影响

- `list_actions` 的 `ActionEvent[]` wire shape 与 UI 行为不变。
- Tauri 命令通过 `ActionService` 读取持久化任务；查询条件、数据库排序与历史投影规则保持不变，未配置数据库时明确失败。
- board 的稳定跨 crate 返回值不再依赖 `serde_json::Value`；动态生命周期事件继续使用现有 Value 适配。
- 不改变数据库 schema、历史记录、命令登记或前端契约。

## 验证

- `cargo fmt -p haven-tools -p haven-app-binary -- --check`
- `cargo check --locked -p haven-tools -p haven-app-binary`
- `cargo test --locked -p haven-tools --lib`
- `cargo test --locked -p haven-app-binary --lib`

测试覆盖后台/定时任务 board 投影、排序、内部字段隔离，以及 DTO 转换后的精确 `ActionEvent` wire JSON。
`ActionService` 持久化查询测试覆盖未绑定数据库错误、kind 过滤和数据库排序；Tauri 命令继续保持现有 history 终态过滤与过滤后限量顺序。

## 回滚与重置

不改变 schema 或 IPC。回滚代码和本 ADR 即可，无需重置用户数据；回滚时应一起恢复 typed board 投影、app 转换与对应测试。
