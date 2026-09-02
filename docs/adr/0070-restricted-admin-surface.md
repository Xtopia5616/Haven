# ADR 0070：受限 Admin Surface 与 capability-scoped 工具

## 背景

`SelfTool` 原本把诊断、配置、技能、builtin tool、MCP 和 session history
全部暴露成一个模型可见的 `haven` dispatcher。除扩大授权面外，任意
`config_set(path, value)` 还把稳定的配置契约退化成 dotted string + JSON，
并可能让模型读到完整日志、prompt 或会话正文。

## 决定

1. 模型目录不再注册 broad `haven` dispatcher，而注册六个 capability-scoped
   工具：`haven_diagnostics`、`haven_config`、`haven_skills`、`haven_tools`、
   `haven_mcp` 和 `haven_session_diagnostics`。每个工具只接受自己的 operation
   allowlist，schema 拒绝额外字段；因此 SafetyGateway 的权限键也按 capability
   和 operation 分离。
2. 删除普通模型路径上的任意 `config_set(path, value)`。配置写操作使用
   `ConfigService::apply_patch(ConfigPatch::...)`；`haven_config` 当前仅保留经过
   typed `level` 校验的日志级别更新，skills/tool/MCP 管理操作也各自使用对应的
   typed patch。后续新增配置能力必须增加显式 patch variant，不得恢复通用 dotted writer。
3. 只读诊断必须有边界：日志行按敏感 marker 脱敏并限制长度；session/error
   诊断只返回 id、状态、时间和字符数，不返回完整输入或 transcript；配置读取
   继续递归 mask API key，MCP 状态不返回环境变量值。
4. 每个 capability operation 声明风险等级、是否只读/可重试和 session scope
   metadata。模型调用仍由 Agent 的 SafetyGateway 在执行前检查，High/Critical
   副作用不会绕过确认或 deny 规则。
5. `SelfTool` 暂时保留为 app command 使用的 native structured surface，**不再
   注册进模型目录**。这是迁移期边界，不是长期兼容层；后续 typed
   `ToolOperation`/admin service 完成后，必须删除 `SelfTool`、`SelfParams`、
   native `run_admin_op` 以及旧文件。

## 替代方案

- 只把原文件机械拆成多个 handler：拒绝，模型仍会获得一个跨域高权限 dispatcher。
- 保留 `config_set` 并增加路径黑名单：拒绝，字符串路径仍不是稳定配置契约，新的
  section 很容易再次绕过审查。
- 每个配置字段立即做一个独立 provider tool：暂不采用，工具数量会膨胀；先用
  capability tool + typed runtime patch，下一阶段再由 `ToolOperation` 完成更细的
  operation contract。

## 影响与验证

- 这是模型工具名与 permission key 的破坏性变化；旧 `haven` 的模型调用和旧
  `config_set` 请求必须重新生成。TOML 配置 schema 不变，不需要删除用户配置。
- native Tauri settings / MCP / skills 命令继续复用同一个 structured surface，避免
  再造第二套持久化逻辑；它们不是模型可见的兼容入口。

重点验证：

```text
cargo fmt --all -- --check
cargo test --locked -p haven-tools self_tool
cargo test --locked -p haven-tools capabilities_have_disjoint_operation_surfaces
cargo check --locked -p haven-tools -p haven-app-binary
cargo clippy --workspace --locked -- -D warnings
```

## 回滚

回退本 ADR 对应提交即可恢复原 `haven` 注册和旧 operation schema；不需要删除
TOML 配置或数据库。若要删除 native `SelfTool`，应在同一提交迁移所有 Tauri
command 调用并更新发布说明中的模型工具契约。
