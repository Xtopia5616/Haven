# ADR 0740：Session permission grant 输出复用授权枚举

## 状态

已采纳并实施。

## 背景

`SessionPermissionGrant` 是 App-owned 的 `list_session_permissions` response DTO。其 `target` / `effect` 来自 `PermissionGrant`，其领域类型已是 Common `PermissionTarget` / `PermissionEffect`，但 App mapper 把它们转换成裸字符串，导致生成 UI contract 也只看到 `string`。目标集合固定为 `operation`、`group`、`tool`；effect 固定为 `allow`、`deny`。`capability` 则是点分层级的动态授权标识，不属于固定 enum。

SettingsSecurity 只按 `effect === 'deny'` 呈现状态，并显示 target 值；该 UI 消费生成的 DTO，没有依赖开放值兼容分支。

## 决定

- 将 App `SessionPermissionGrant.target` / `.effect` 改为 Common `PermissionTarget` / `PermissionEffect`，mapper 直接传递领域值。
- generated response 类型复用现有 `PermissionTarget` / `PermissionEffect` unions，保持序列化 snake_case 字面量不变；capability 仍为 string。
- 添加序列化测试固定 `group` / `deny` 的既有 JSON 形状。

## 替代方案

- 保留 App 字符串投影：拒绝。它重复了 Common enum 的固定词汇并丢失了编译期保证。
- 在 App 重新定义 grant 专用 enum：拒绝。与授权 Common owner 重复表达相同的策略值。
- 将 capability 一并枚举化：拒绝。capability 是任意合法层级标识，由授权名称策略验证，不是封闭集合。

## 影响与验证

生成的 `SessionPermissionGrant` 从开放 `target: string` / `effect: string` 收窄为相应 Common enum unions。现有 UI JSON 字符串、字段名和授权行为保持不变；无数据库、配置、权限 key、迁移或重置语义变化。

验证：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked` 通过；UI `check`、`test:run`（125 files / 992 tests）、`build` 通过；IPC command contract 检查（80 handlers）、IPC event 检查（35 channels）、ADR index（723 records）及 `git diff --check` 通过。

## 回滚

如回滚，恢复 App DTO 的字符串字段和 mapper 的 `.as_str()` / `match` 投影，并恢复生成的开放字符串字段；无需更改授权领域类型或持久数据。
