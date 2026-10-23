# ADR 1380：注册表 typed 资源边界

## 状态

已实施 — 2026-10-23。

## 背景

`system.registry` 只有只读的名称枚举和单值读取操作，但授权层此前无法区分 Registry hive、key、列举与读取，也不能把持久 allow 限定到用户确认的目标。通用系统信息权限会把访问不同 hive 和值的行为混为一项能力。

## 决定

1. Registry adapter 在请求授权前，将短名、完整 hive 名和 PowerShell 路径别名规范化为固定 hive（`HKCU`、`HKLM`、`HKCR`、`HKU`、`HKCC`）及 key 子路径；不支持的 hive 和含 NUL 的路径拒绝执行。
2. `list` 声明 `RegistryAccess::EnumerateNames`，只返回值名、子键名与截断状态；`get` 声明 `RegistryAccess::ReadValue`，并绑定一个非空的精确 value name。列举不授权读取值内容。
3. 执行时重算资源声明，并要求它与 proof 完全一致；有资源的调用必须通过 proof-derived execution permit。Registry 的持久授权只允许 exact scope，精确匹配 hive、key、访问类型和值名，不支持目录式继承。
4. UI 在确认和授权设置中分别显示列举名称与读取指定值，包含目标 hive/key；读取授权同时显示 value name。

## 配置与数据影响

数据库、工具参数和默认值不变。安全配置的 `PermissionResourceScope` 增加 `registry` 判别分支，属于 additive 配置序列化变更，无需重置配置或数据库；IPC typed union 与授权 UI 描述同步更新。

## 替代方案

- 只按 `system.registry` 能力授权：拒绝；无法限制到用户确认的 hive/key。
- 让名称枚举同时许可读取值：拒绝；名称元数据和注册表值内容是不同访问方向。
- 对 Registry grant 提供子树/目录匹配：拒绝；这会将一项确认扩展到未确认的 key 集合。
- 将 Registry value 内容放进授权身份：拒绝；内容可变化且可能敏感，授权只绑定被读取的 value name。

## 验收与限制

测试覆盖路径别名规范化、非法 hive/path/value name 拒绝、持久授权的 hive/key/访问/value name 精确匹配，以及 UI 的授权描述。Registry 对真实 Windows 配置的读取、名称枚举上限和阻塞线程取消仍由 Windows 发布验收门禁验证；访问权限受当前 Windows 用户令牌限制。
