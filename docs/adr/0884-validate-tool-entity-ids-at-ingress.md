# ADR 0884：在 Tools 入口校验模型提供的实体 ID

## 状态

Accepted — 2026-10-10

## 背景

ADR 0883 收紧了 App 命令与凭据引用的 ID 入口，但模型也能直接提供 ToolRun 和受管媒体资产 ID。部分 schema 仅要求非空字符串；`Tool::run` 原生路径还可以绕过 JSON schema。错误格式可能被误报为“未找到”，而 `watch_tool_run_id` 会进入定时任务的依赖记录。

## 决定

- Tools 通过 `tool_contract::validate_entity_id` 调用 Common `is_canonical_id`；格式判断不在各工具内重写。
- `tool_runs` 与 `schedule` 在查询、取消或保存 watcher 关系前校验 `toolrun-*`；`ScheduledTriggerRequest` 在 ToolRunService 接受该关系时再校验一次，确保服务入口不会保存非规范依赖 ID。
- Media、Files、Clipboard 在解析受管资产或写入剪贴板前校验 `asset-*`。
- 模型 schema 同时描述对应前缀和 UUID32 形状，以便 provider 在调用前发现无效参数；运行时的 Common 校验仍是权威边界。
- 不为旧格式保留兼容解析。ToolRun-watch 的数据库关系与运行时 watcher 的恢复生命周期仍由既有 ToolRun 决策管理，本 ADR 不改变重启恢复语义。

## 影响与兼容性

符合规范的 ID 和成功结果不变。格式错误、错误前缀、大小写不规范或 UUID 长度错误的模型参数现在在副作用前被拒绝；规范但不存在的 ID 仍按原有 not-found 语义处理。无数据库 schema 变更，无需重置数据。

## 验证

通过：`cargo fmt --all -- --check`、`cargo test --locked -p haven-tools --lib --quiet`（803 passed、2 ignored）与 `git diff --check`。完整 workspace tests、Clippy 及 IPC/UI 门禁在本轮最终验证中执行。

## 回滚

若规范生成的 ToolRun 或 asset ID 被拒绝，应修复实体 owner 的生成规则或前缀定义；不得恢复非空字符串校验或工具本地 UUID 解析器。
