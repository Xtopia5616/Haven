# ADR 0337：Settings runtime apply 计划与阶段协调

- 状态：已采纳（2026-09-25）
- 范围：`haven-app-binary` 的 `update_settings` runtime apply 阶段规划与观测
- 关联：[ADR 0235](0235-versioned-runtime-config-apply-boundary.md)、[ADR 0323](0323-model-config-apply-coordinator-ownership.md)、[ADR 0324](0324-settings-apply-phase-failure-observability.md)

## 背景

ADR 0324 将 Settings phase tracker 和失败元数据移入 `config_runtime`，但命令仍逐段判断 target、手动保持调用顺序，并在多个位置更新 tracker。security、MCP、context、logging 与 hotkey 等阶段因此还没有共享的 typed apply plan。与此同时，Router/model 的 `RuntimeConfigCoordinator` 已拥有配置串行 gate 及 Router/media prepare→publish；Settings 需要收口自己的计划与观测，而不复制这套运行时 owner。

## 决定

1. `SettingsApplyPlan` 从现有 `RuntimeConfigApplyPlan` 派生 live/restart-required targets，并列出 Settings 阶段顺序。target 映射继续共用，不建立第二套 config source 或重复 domain mapping。
2. `SettingsRuntimeApplyCoordinator` 拥有 Settings 阶段执行顺序、当前 phase、snapshot version、Router published 状态、restart-required targets，以及失败/警告记录。它按 typed plan 调用 command 提供的 phase callback；失败立即停止后续阶段。
3. `RuntimeConfigCoordinator` 仍拥有共享 `config_apply_gate`、model durable edit 和 Router/media prepare→publish。Settings coordinator 只调用这些现有接口，不预构建或发布另一份 Router runtime。
4. 实际副作用仍由原 owner 执行。顺序保持为：Router prepare → input pipeline → shell → security → MCP config → MCP monitors → Router publish 与 `llm:config_changed` → context limits → session runtime → tool settings → skills → logging → hotkey mode → hotkey unregister → hotkey register → hotkey rebind event。Router prepare 和 publish target 仍来自共享 target plan；hotkey unregister/register/event 仅在同次 edit 捕获的旧 binding 与新 binding 不同时进入 plan。
5. `update_settings` 仍在同一次 `ConfigService::edit` 中读取旧 hotkey 并合并 Settings。edit 返回 no-op 时在创建 Settings runtime coordinator 前成功快返。permissions、`encrypt_sensitive` 与表单不管理字段仍由 `AppConfig::apply_settings` 保护。
6. 错误 renderer 和观测保持 ADR 0324：Router/media prepare 的内层 builder 已通过 `log_err` 的错误原样返回；Skills、logging 和 hotkey 注册错误仍使用各自既有 command context 调用 `log_err`，再附加脱敏的结构化 phase/version/Router/restart 元数据。hotkey rebind event 失败仍是 warning-only。

## 失败语义与未决事项

- `ConfigService::edit` 先持久化并发布 versioned `ConfigChanged`。no-op 不持久化，也不运行 apply coordinator。
- Router prepare 失败时，新配置仍已持久化；尚无 Router/media publish、也不继续其他 Settings 阶段。
- prepare 成功后，后续阶段按上列顺序逐一执行。Skills、logging 或 hotkey 阶段失败时，durable 配置和更早已完成的 runtime 副作用保留，不做补偿。
- hotkey unregister 成功而 register 失败时，不恢复旧 binding。Router published 与 restart-required targets 仅用于准确观测，不表示恢复成功。
- 完整 compensation/rollback、失败后的 restart recovery，以及接受半应用状态的产品策略仍未决。本 ADR 不增加逆操作、配置回滚、live snapshot 恢复或启动恢复机制。

## 替代方案

- 将 Settings 所有 runtime 实现复制到 `RuntimeConfigCoordinator`：会把 Router/model owner 与 security、MCP、logging、hotkey 等应用组合混成一个更宽边界，拒绝。
- 在 command 继续手动分支并记录阶段：会让计划顺序和观测状态继续有多处 owner，拒绝。
- 把 apply 包装成事务并在失败时回滚：当前副作用没有共同 prepare/commit 和可靠逆操作，拒绝；策略留待单独 ADR。

## 影响与验证

配置 schema、IPC、数据库、ConfigService edit/no-op、`Result<(), String>`、敏感配置保护、Router prepare→publish、side-effect order、失败文案和既有半失败行为均不变。测试覆盖 plan 顺序与 target 派生、no-op 快返、每个阶段的成功/失败状态、Router publish metadata、restart targets、失败 renderer 和敏感值不进入日志。

验证命令：`cargo fmt --all -- --check`、`cargo test --locked -p haven-app-binary`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`、`git diff --cached --check`。

无需配置或数据库重置。回滚本切片只需恢复 Settings 命令的阶段入口和旧 tracker；不涉及用户数据。
