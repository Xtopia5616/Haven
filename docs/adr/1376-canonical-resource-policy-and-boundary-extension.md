# ADR 1376：统一资源策略与本次边界扩展

## 状态

已采纳，实施完成 — 2026-10-23。

## 背景

ADR 1374/1375 已让 Files 与 HTTP 声明 typed 资源，并将可复用授权绑定到资源范围；但配置仍分别使用 `sandbox_mode`、`writable_roots`、`network_policy` 和工具 `allowed_paths`。设置页把其中部分折叠到高级区域，没有一个规范策略描述其组合，也没有安全地处理“本次操作需要触及当前范围外资源”的路径。

## 决定

1. 在 `haven_common` 定义唯一 typed `ResourcePolicy`，用于全局安全默认值和工具级进一步收窄。策略按资源种类表达文件读写范围、网络目的地范围及其限制模式；执行工作目录仍是路径解析基准，不等同于访问授权。工具级规则必须与全局策略取交集，不能扩大它。
2. `SecurityConfig` 持有全局 `resource_policy`；`ToolConfig` 使用同一资源规则模型表示工具级限制。工具启用、重试、风险覆盖、禁用 operation 等非资源设置仍由 `ToolConfig` 持有。序列化只写规范结构；读取旧配置时一次性把 `sandbox_mode`、`writable_roots`、`network_policy`、`allowed_paths` 及工具域名限制转换到新结构。下一次保存不再写旧字段，不维护运行时影子配置。
3. `AuthorizationEngine` 是唯一资源策略决策 owner。它以工具可信声明的 typed resources 计算有效边界；不得扫描动态 JSON 参数推断资源。deny、禁用操作、只读文件边界、全局禁网及 Analyze 是硬拒绝，普通授权和边界扩展均不能覆盖。
4. `WorkspaceWrite` 写目标超出当前会话工作目录或配置写入目录时，Collaborative 模式产生资源范围绑定的确认请求。确认展示确切 typed 目标，并说明本次会临时扩展写入范围；获准后仅对该精确调用签发带扩展标记的执行 proof，副作用前仍重新校验 proof、策略 revision、路径身份和资源声明。只读、工具级路径限制及禁网仍是硬边界。此类确认只提供 Once：Session/Always grant 不能隐式改写边界。用户要持久扩大范围时，必须直接编辑并保存规范 `ResourcePolicy`。
5. 缺少可信资源声明、资源类型未实现、策略配置无效、隔离能力无法落实或发生策略竞态时一律 fail closed。临时扩展不自动重试副作用已启动或结果未知的操作。
6. 设置页以一个“资源策略”区域编辑全局文件/网络范围并展示有效写入边界；工具级限制使用同一 `ResourcePolicy` schema，仍作为更窄的硬限制执行。执行工作目录与执行方式继续留在各自入口。持久修改仍走现有 Settings patch/apply owner。

## 配置、兼容与数据影响

旧 `config.toml` 在载入时转换到 `ResourcePolicy`，不丢弃现有根目录、网络限制或工具路径限制；原有权限 allow/deny 保持不变。保存后只写新配置结构。回滚旧二进制前需恢复升级前的 `config.toml` 备份，因为旧版本不理解新的 `resource_policy` 字段。此配置改动不要求重置数据库。

临时边界扩展只存在于已接受确认的 typed receipt/proof 生命周期，不写配置或数据库，也不产生可复用 grant。现有 `session_events` 中新增 receipt 标记按可选字段读取；数据库 schema 不变。旧二进制恢复时按 ADR 1374/1375 对数据库和配置的既有回滚规则处理。

## 替代方案

- 只把旧设置折叠进高级 UI：拒绝；权威数据和组合规则仍分散，工具策略也不能被完整说明。
- 把一次确认写成全局可写根或域名设置：拒绝；一次操作会暗中改变后续调用的能力。
- 让永久授权绕过任何资源边界：拒绝；持久 grant 是能力授权，不能取代明确的持久策略修改。
- 让 UI 根据工具名或参数字段猜测越界资源：拒绝；资源身份必须来自后端 typed contract。

## 验收

- 新配置只序列化 `security.resource_policy` 与工具级 typed resource rules；旧格式完整读取并转换，保存后不再出现旧边界字段。
- 全局与工具级策略按交集执行；修改/重载会推进策略 revision 并撤销受影响的运行尝试。
- 硬拒绝优先于资源范围 grant 与临时扩展；Analyze/Unattended 不会因扩展请求而执行。
- 只有有可信 typed 资源的可扩展越界可请求 Once；确认绑定确切资源、输入、策略和期限，拒绝、过期、重放、路径替换、策略变化均不执行。
- 设置页以规范 `ResourcePolicy` 编辑全局策略并展示有效边界及扩展语义；工具级配置也使用同一 schema，作为更窄的硬限制参与执行。
- `cargo check --workspace --locked`、IPC 契约生成和 `corepack pnpm run check` 通过；本轮未运行测试。Windows 路径/重解析/竞态运行时验收仍需在后续安全门禁中完成；配置回滚按本文执行。
