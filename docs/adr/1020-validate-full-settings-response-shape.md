# 1020：从 Rust 契约校验完整 Settings 响应

## 状态

已采纳并实施（2026-10-12）。

## 背景

`get_settings` 的 UI parser 只检查根对象、已知凭据引用和数值；缺少必需嵌套字段的对象仍会被断言为完整 `Settings`。`SettingsView` 随后以本地默认值填充缺失分组，并允许保存完整快照，可能把响应缺失字段覆盖回后端。

## 决定

- IPC generator 从 Rust `Serialize`/Serde 类型图生成递归 `SETTINGS_RESPONSE_SHAPE`，唯一来源为 `haven_common::config::Settings` 及其嵌套类型。
- Schema 反映响应序列化语义：必需字段保持必需；`skip_serializing_if` 字段可省略；未跳过的 `Option` 是必需 nullable；跳过序列化的字段不进入 shape；`serde(default)` 不改变响应必需性。
- 结构与闭枚举由通用 UI shape evaluator 校验。对象仍允许未知附加字段，动态 `serde_json::Value` 保持开放。
- `parseSettingsPayload` 继续执行凭据引用和数值域检查；结构不完整或字段类型/闭枚举无效的对象作为无效响应拒绝。null/非对象保留现有空结果语义；无效响应不能开启完整快照保存。
- `check-ipc-contracts.ps1` 要求 generated schema 存在且 parser 使用该 schema。

## 影响

后端 wire 字段、配置文件和持久化格式不变，不需要重置。非法或不完整的 `get_settings` 结果会进入现有加载错误处理，Settings 页面不允许用 fallback 保存覆盖数据；新增响应字段保持向前兼容。

生成后的 `generatedCommands.ts` 因内联完整 Settings shape 超过 800 行；该文件保留为单一生成产物，使 DTO、command map 与运行时 shape 由同一 generator 原子校验，不添加人工维护的第二份 schema。

## 替代方案

- UI 手写完整嵌套 `Settings` schema：拒绝；这会复制 Rust 配置模型并引入漂移源。
- 只验证根对象或手动枚举少数关键字段：拒绝；仍无法保证完整快照保存安全。
- 禁止未知附加字段：拒绝；会破坏当前配置响应的开放扩展语义。

## 验证

- Rust codegen 单测覆盖 Serde required/optional/nullable/skip/default、嵌套 DTO、枚举、数组、map、transparent wrapper、动态 JSON 和递归拒绝。
- UI contract tests 覆盖完整 Settings fixture、不完整嵌套记录、primitive/enum 异常、unknown additive fields、credential/numeric 语义及 nullable response 字段。
- SettingsView 回归验证不完整响应不能开启保存。
- IPC generation/check、Rust workspace gates、UI check/tests/build、ADR index 与 diff checks。

## 回滚

移除 generated runtime shape 和 UI matcher 会恢复不完整对象通过 parser 的行为。当前实现不改变持久数据或配置格式。
