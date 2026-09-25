# ADR 0324：Settings runtime apply 阶段与失败可观测性

- 状态：已采纳（2026-09-25）
- 范围：`haven-app-binary` 的 `update_settings` runtime apply 失败诊断
- 关联：[ADR 0235](0235-versioned-runtime-config-apply-boundary.md)、[ADR 0253](0253-runtime-config-coordinator.md)、[ADR 0323](0323-model-config-apply-coordinator-ownership.md)

## 背景

`update_settings` 先通过 `ConfigService::edit` 持久化新配置，再从同一 snapshot 构造 plan 并按既有顺序应用 live runtime。Router prepare 失败会在任何 live publish 前返回；prepare 成功后，skills、logging 或 hotkey 阶段失败不会撤销已保存配置和前面已经完成的副作用。原错误 renderer 会脱敏错误文本，但只记录 command/error，无法说明 snapshot 版本、失败阶段、Router 是否已发布或本次是否还含有 restart-required target。

## 决定

1. `SettingsApplyPhaseTracker` 由 `config_runtime` 所有。settings edit no-op 在创建 tracker 之前直接返回；发生 change 后 tracker 以 snapshot version 和 restart-required targets 初始化。
2. tracker 按当前调用链命名阶段，并通过 `RuntimeConfigApplyPlan::contains` 判定目标，与旧分支条件保持一致。阶段顺序为：Router prepare → input pipeline → shell → security → MCP config → MCP monitors → Router publish 与 `llm:config_changed` event → context limits → session runtime → tool settings → skills → logging → hotkey mode → hotkey unregister → hotkey register → hotkey rebind event。
3. 错误日志增加结构化字段：`config_version`、`phase`、`router_published`、`restart_required` 和 `restart_required_targets`。错误详情经 `sanitize_error_text` 清洗；command `Result<(), String>` 与现有 `log_err` renderer 保持不变，当前用户可见错误文本不重写。
4. Router prepare 仍必须先于任何 live publish；prepare 失败时不执行后续 runtime 阶段。成功 publish 后 tracker 将 `router_published` 标为 true。hotkey rebind event 仍为 warning-only，但 warning 也带 phase/version 状态。
5. 本决策只增加阶段所有权和失败可观测性。durable config 不回滚，已完成的 live 副作用不补偿，也不宣称 settings 是原子事务。skills/logging/hotkey 的失败仍可能留下半应用状态。

## 失败矩阵

| 阶段 | 失败时已发生的状态 | 记录信息 |
|---|---|---|
| durable edit | ConfigService 保持原有 edit 失败语义；尚未创建 apply tracker | 沿用现有 config edit error renderer |
| Router prepare | 新 snapshot 已保存；无 live publish；后续 settings 阶段未执行 | version、`router_prepare`、Router 未发布、restart-required targets |
| pipeline / shell / security / MCP / Router publish / context / session / tool settings | 这些当前接口不返回可传播的 apply error | 阶段顺序与副作用保持现状 |
| skills / logging | 新 snapshot 及此前完成的副作用保留；不回滚 | version、失败 phase、Router 发布状态、restart-required targets 和脱敏错误 |
| hotkey unregister / register | 此前副作用保留；unregister 成功但 register 失败时不恢复旧绑定 | version、具体 hotkey phase、Router 发布状态、restart-required targets 和脱敏错误 |
| hotkey rebind event | runtime apply 仍成功返回；事件失败只记 warning | version、`hotkey_rebind_event`、Router 发布状态、restart-required targets 和脱敏错误 |

## 替代方案

- 将 settings 全部副作用包装成虚假事务：副作用没有共同 prepare/commit 或可靠逆操作，拒绝。
- 改写 command 错误文本来拼接 phase 信息：会改变现有 renderer contract，拒绝。
- 只在 command 内堆叠字符串上下文：仍无法保证 Router publish 状态及 restart-required 信息一致，拒绝。

## 影响与验证

配置持久化、schema、IPC、运行时副作用顺序和 `Result<(), String>` 均不变。测试覆盖 Router prepare 失败不 publish、Router publish 后错误含 version/phase metadata、restart-required target 保留、no-op 不创建 phase runner；测试通过 tracker/coordinator 的纯内存边界完成，不需要 Tauri 外部资源。

验证命令：`cargo fmt --all -- --check`、`cargo test --locked -p haven-app-binary --lib`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`、`git diff --cached --check`。

## 回滚

回退本切片可移除 phase tracker 和结构化日志，不需要重置数据库或用户配置。Settings 完整补偿/回滚策略仍需单独决定和设计。
