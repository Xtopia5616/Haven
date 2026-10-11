# ADR 0978：让 CapabilityScope 只通过校验构造

## 状态

已采纳并实施。

## 背景

`CapabilityScope::try_new` 和 JSON 反序列化已经检查空值、首尾空白、空层级段与非 ASCII/保留分隔字符，但公开 `new` 只用 `debug_assert!`，公开 `From<&str>` / `From<String>` / `From<&String>` 完全绕过验证。`AuthorizationEngine::grant` 与 `OperationPolicy::external` 接受 `Into<CapabilityScope>`，因此调用方可把不规范权限键送入授权状态。

## 决定

1. 删除公开 `CapabilityScope::new` 及从字符串到 `CapabilityScope` 的 `From` 实现；外部值只能通过 `try_new` 或受同一校验保护的反序列化创建。`From<CapabilityScope> for String` 保留，因为它只导出已校验值。
2. capability ancestry 和 target 只由已有合法 scope 派生，并通过类型内部私有构造 helper 表达该不变量。
3. `AuthorizationEngine::grant` 和 `OperationPolicy::external` 改为显式接收 `CapabilityScope`，不再隐式接受任意字符串。`revoke_permanent` 解析边界字符串，格式无效时返回 `false` 且不修改任何授权规则。
4. 每次调用的工具策略解析返回 `Result`。Capability 构造失败必须沿授权端口传递为拒绝/错误；在授权引擎决策与工具执行前失败关闭。定时调用、恢复确认和 catalog snapshot 授权使用相同规则。仅供清单展示的非授权策略投影发生错误时使用高风险、独占的保守展示值。
5. 删除因非空校验而存在的 `CapabilityScope::is_empty`；session grant 存储边界依赖类型构造不变量，避免重复检查同一条件。

## 替代方案

- 将 `new` 改成对无效值 panic：拒绝；错误输入应走 `try_new` 的可恢复错误路径。
- 保留 `From` 并只校验配置读取：拒绝；其它 Rust 调用方仍可在安全授权入口绕过配置加载。
- 在授权失败时回退到祖先 scope：拒绝；更宽的 scope 可能意外匹配已有 grant。

## 影响与验证

- Common、Tools、Agent、App 与 Memory 调用点统一显式构造已校验 capability；JSON wire shape 和数据库文本存储格式不变。
- Common 测试继续覆盖合法 hierarchy、无效输入和反序列化；Tools/Agent 测试验证 schema 可通过但 capability 不合法时 resolver 返回错误，且工具不会执行；授权测试验证畸形永久撤销不会更改已保存规则。
- 不改权限层级匹配、持久化 schema 或 IPC 字段。

## 回滚

恢复公开无校验 constructor/conversions 与旧授权参数签名，并同步回退调用点、测试、命名规范、路线图及 ADR 索引；无需数据迁移。
