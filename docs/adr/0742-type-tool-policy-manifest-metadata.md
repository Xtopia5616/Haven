# ADR 0742：Tool manifest 策略元数据复用闭合类型

## 状态

已采纳并实施。

## 背景

`ToolPolicy` 是 Common 定义的 manifest IPC 投影。确认方式、幂等性、作用范围、并发类别、操作效果、数据敏感级别和网络访问级别均由 Tools 的固定 enum 产生，但 DTO 与生成的 TypeScript 把它们声明为 `string`；`toolManifest.ts` 也只验证这些字段是非空字符串。未知词汇因此可进入 renderer。`risk_level` 已是 enum。`permission_key` 则是层级 capability 标识，必须保持开放字符串。

运行时 `ToolConcurrency` 的 shared/resource 变体带有调度资源 key，而 UI manifest 只需要并发类别。当前 projection 已故意不序列化该 key，只输出四个固定类别。

## 决定

- 将 `ConfirmationRequirement`、`OperationIdempotency`、`ToolOperationScope`、`OperationEffect`、`DataSensitivity` 与 `NetworkAccess` 放在 Common，供 runtime operation policy 与 Common `ToolPolicy` 共同使用。
- 增加 Common `ToolConcurrencyMode` 作为 manifest 投影类型；Tools 将 runtime `ToolConcurrency` 映射为 `read_only`、`shared_resource`、`resource` 或 `exclusive`，不暴露资源 key。
- 将这些 enum 用于 `ToolPolicy` 对应字段，Serde 保持现有 snake_case 字面量。UI 的静态 view 和 runtime parser 消费生成类型和值清单，并拒绝未知成员。
- `risk_level` 继续使用 `RiskLevel`；`permission_key` 继续使用开放字符串。MCP/Skill/Builtin 的执行边界和 manifest 外层 shape 不变。

## 替代方案

- 保持 metadata 字段为 `String` 并接受未知值：拒绝。当前生产 owner 已输出有限枚举，renderer 不依赖对未知策略类别的透传。
- 把 runtime `ToolConcurrency` 直接序列化：拒绝。其资源 key 属于 Agent 调度实现，不是 UI manifest 契约。
- 为 manifest 另定义一套字符串映射 enum：拒绝。确认、幂等、scope、effect、sensitivity 和 network 类别与 operation policy 是同一语义，应共享 Common owner。
- 将 `permission_key` 也枚举化：拒绝。其 capability 名称是可扩展的层级标识，不属于闭合集合。

## 影响与验证

JSON 字段和值保持不变；生成的 `ToolPolicy` 与 `ToolManifestView` 从开放字符串收窄为生成 enum union，缺失或未知的策略类别会使 UI manifest parser 拒绝该条目。Common `ToolPolicy` 不再为 effect/sensitivity/network 字段补默认空字符串，缺少这些必需字段的反序列化也会失败。没有数据库、配置、迁移、恢复或数据重置变化。

验证：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked` 通过；UI `check`、`test:run`（125 files / 993 tests）、`build` 通过；IPC command contract 检查（80 handlers）、IPC event 检查（35 channels）、ADR index（725 records）及 `git diff --check` 通过。

## 回滚

如回滚，恢复 `ToolPolicy` 的字符串字段与 Tools projection 的 `.as_str()` 映射，并恢复 UI 的非空字符串解析。移除本 ADR 和对应 generated enum 引用；无需改动工具执行策略、数据库或用户数据。
