# ADR 0696：生成 App shell 事件契约并共享校验

## 状态

已采纳并实施。

## 背景

`contracts/app.ts` 的 `AppWirePayloadMap` 重复声明了应用启动、托盘、静音、MCP、Skills 和 hotkey 事件的 wire shape；对应 Rust event DTO 中启动、托盘和 Skills 状态还以 `String` 表示。UI 对启动/托盘/Skills 值同时维护 type union 与运行时校验数组。交互风险等级已有 Rust 生成值列表，但 app mapper 和 resume reducer 又各自维护一份。MCP status 类型已生成，app event mapper 与 MCP command response 仍重复实现同一运行时校验。

## 决定

- App shell 的稳定事件 DTO 由 IPC generator 显式生成，UI 对未变换的 payload 直接引用生成 DTO；删除 `AppWirePayloadMap` 和仅重复 wire 字段的 UI payload types。
- Rust `BootstrapStatus`、`TrayStatusEventValue`、`SkillsStatusOperation` 使用具名枚举；`TrayStatusEventValue` 通过显式转换映射内部 `desktop::TrayStatus`，保留小写 wire 值。
- IPC generator 为 `McpClientStatus` 额外导出来自 Rust 外部标记 enum 的无载荷 variant 清单。App event 和 MCP command response 共用一个 UI validator。
- App mapper 与 session reducer 共用生成的 `RISK_LEVEL_VALUES`；闭合状态值校验使用生成值清单。
- `interaction:requested` 与 `hotkey:rebind` 继续保留 renderer 投影，因为它们分别执行 owner/casing 转换；其它事件 DTO 不增加手写 wire 层。

## 替代方案

- 保留 App wire map 并继续手工同步 Rust 字段和值：拒绝。map 不增加运行时校验，只建立第二份容易漂移的 shape 声明。
- MCP event 与命令响应分别保留 status validator：拒绝。两处消费同一 Rust enum，且结构规则完全一致。
- 把 `desktop::TrayStatus` 直接序列化为事件值：拒绝。内部 shell state 的 Serde 表示与 renderer 事件的小写值有不同角色；显式 event enum 保留边界转换。

## 影响与验证

- Tauri event JSON 保持原样；命令响应 shape、事件映射拒绝规则、持久化数据和安全行为不变。无需数据库或配置重置。
- Rust/TS 生成契约和值清单来源于 Rust DTO/enum；UI event mapper 继续验证不可信 payload，并只在 Interaction 与 hotkey rebind 边界进行 renderer 投影。
- 验证：`cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`scripts/check-ipc-contracts.ps1`、UI `check`、`test:run`、`build`、ADR 索引检查与 `git diff --check`。

## 回滚

恢复原有 Rust string event fields 与 UI payload map/validator 即可；没有数据库、配置或事件 JSON 迁移。
