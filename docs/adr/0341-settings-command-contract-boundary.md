# ADR 0341：Settings 命令 contract 边界收口

- 状态：已采纳（2026-09-25）
- 范围：Settings `get_settings` renderer ingress 与 `hotkey:rebind` event contract 审计
- 关联：[ADR 0007](0007-settings-diagnostics-contracts.md)、[ADR 0330](0330-session-lifecycle-ui-contract-mapper.md)、[ADR 0335](0335-action-board-ui-contract-mapper.md)、[ADR 0340](0340-recording-event-contract-audit.md)

## 背景

全量设置的 Rust wire DTO 是 `haven_common::config::Settings`，由 `settings_pair!` 与 `AppConfig` 的共享字段表定义；TS 没有另一份全量嵌套 interface。此前 `SettingsView`、`+layout.svelte`、`+page.svelte` 和 `chatModelSync` 多处直接 `invoke('get_settings')`，并各自读取 wire 数据。Settings diagnostics 的 `get_log_info`、`read_log_tail`、`check_shell_available`、`get_api_key_status` 已在 `contracts/settings.ts` 有唯一 parser（ADR 0007）。`hotkey:rebind` 的 Rust DTO 则已由 `contracts/app.ts::mapAppEvent` 单点映射为 camelCase。

审计 `settingsSaveAction`、`settingsGuard` 与 settings 状态后确认：设置表单只有 `SettingsView` 一处 owner；没有独立 settings store。`buildPersistableSettings` 是 dirty-check 用的本地快照，不是第二个 IPC serializer；`update_settings` 的 payload 也只在 `SettingsView` 构造一次。

## 决定

1. 所有 `get_settings` 读取统一经 `ui/src/lib/settingsCommand.ts::loadSettings`，唯一调用 Tauri command，并调用 `contracts/settings.ts::parseSettingsPayload` 做根对象运行时校验。
2. Rust `Settings` 继续是全量配置形状的唯一结构定义。前端 validator 不复制嵌套 schema、不重建 payload，而是透传对象，以保留现存 snake_case 字段、未来配置字段和未知枚举字符串。
3. null、数组和 primitive 根值归一为 `null`，保持原页面 optional access / no-op 行为；Tauri command rejection 原样透传到原有 catch，所以日志、用户提示及异步通知顺序不变。
4. `hotkey:rebind` 继续只由 `contracts/app.ts::mapAppEvent` 将 `old_binding` / `new_binding` 转为 `oldBinding` / `newBinding`。新增回归覆盖确保扩展 wire 字段不进入消费 DTO。
5. `settingsSaveAction`、`settingsGuard`、诊断 response parsers 和 SettingsView 的唯一保存 payload 保持原有职责。本切片不改 Rust DTO、命令名、事件 channel、IPC 字段、保存/通知顺序，也不引入全局 codegen。

## 必须保持的不变量

- Settings form、hotkey/model sync、布局通知偏好加载等 `get_settings` consumers 都通过同一入口解析；页面不可直接处理未经 validator 的原始 command response。
- 未知嵌套字段和字符串枚举保持原值；此入口只判定 JSON object root，不把配置扩展点变成封闭枚举或忽略未知配置。
- malformed root 值保持既有无操作式降级；Tauri invoke 错误保持原对象/错误文本并按现有调用方 catch 处理，既有错误通知与日志顺序不变。
- Settings save、API-key status refresh、成功 toast、autostart 和 snapshot capture 的既有先后次序不变。
- hotkey event 的 Rust snake_case 到 UI camelCase 映射点唯一；监听器仍按原有 events/app mapper 路径交付。

## 替代方案

- 在每个页面复制 `get_settings` 响应检查：继续形成多个 wire 入口，拒绝。
- 在 TS 维护全量 Settings 嵌套 interface，并逐字段重建对象：会复制 Rust config schema、收窄未知扩展行为，并扩大本切片影响，拒绝。
- 将 snake_case config DTO 整体迁为 camelCase：会改变 Settings view、model sync 与保存映射，超出 contract-ingress 收敛范围，拒绝。
- 引入 Rust→TS 全局 codegen：Phase 8 尚无跨全部 DTO 域的统一生成策略；当前仅收口一个运行时入口，拒绝。

## 影响与验证

command、event wire payload、Rust config serialization、权限敏感字段保护与 runtime apply 行为不变；不涉及 schema、持久化数据或重置。新增 tests 覆盖单一 command 调用、未知字段/枚举透传、malformed root no-op 和 hotkey event 字段映射。命令 rejection 在 helper 不做 catch/wrap，调用方仍沿既有 catch 处理。

验收命令：

```sh
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
corepack pnpm --dir ui run build
```

## 回滚

回滚本切片可恢复各调用方直接执行 `get_settings`；不需要 Rust 变更、配置迁移或数据重置。
