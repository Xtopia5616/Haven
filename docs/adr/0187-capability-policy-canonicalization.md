# ADR 0187：能力标识与授权边界的单一契约

- 状态：accepted
- 日期：2026-09-21
- 范围：`haven-common`、`haven-tools`、`haven-agent`、`haven-app-binary`、Settings UI

## 背景

权限实现已经具备确认模式、文件沙箱、网络策略、永久规则和会话授权，但边界仍有
三处不一致：能力身份同时使用 `root.operation` 和 `root:operation`；禁用操作从原始
JSON 参数重新推断 scope/operation；MCP/Skill 的 opaque 网络边界由授权引擎按工具名
猜测。这样会让目录、配置、确认收据和执行 gate 对同一次调用得出不同结论。

## 决定

1. capability identity 统一使用点号层级：`files.read`、`system.power.lock`。父级继承
   只沿点号 ancestry 匹配，旧冒号配置继续作为破坏性 reset 边界，不在运行时猜测迁移。
2. `CapabilityScope::try_new` 是外部规则的校验入口；运行时加载配置时再次校验，非法规则
   被忽略并记录净化后的 key，不能形成可执行授权。
3. `disabled_operations` 按最终 `OperationPolicy.capability` 匹配。保留以工具设置名为
   前缀的短写（例如 `tool_settings.files = ["read"]`），但不再读取任意输入字段决定
   哪个 operation 被禁用。
4. `OperationPolicy.network_access` 是网络边界的唯一声明。MCP/Skill adapter 直接声明
   `Opaque`；`AuthorizationEngine` 不再通过 `mcp__`、`skill__` 或 admin 工具名推断网络
   能力。未经声明的 native 入口必须显式构造 `NetworkAccess`。
5. 授权结果携带 `AuthorizationReasonCode`，错误文案只用于展示。调用方不得通过解析文案
   判断是禁用、沙箱、网络、规则、Plan、Critical 还是普通确认。
6. 设置页把确认模式、技术边界和持久化规则收敛到 `SettingsSecurity`，普通设置页面只
   负责快照与保存，不再持有权限规则的展示细节。

## 不变量

- 所有进入执行层的请求只有一个 canonical capability；确认 receipt、永久规则、会话规则、
  disabled operation 和 UI renderer 使用同一 identity。
- 技术边界和 deny 规则先于 allow/确认模式；规则不能降低 Critical、Plan、sandbox 或
  network 的安全底线。
- renderer 继续只接收后端摘要和 reason code，不接收原始 shell、URL、路径内容或扩展参数。

## 验证

覆盖 common 的点号 key/层级与非法输入测试、tools 的非法持久化规则、disabled operation
与任意 discriminator 不一致的负例、network deny 对 HTTP/MCP/Skill 的负例、reason code
稳定性，以及 UI 的 `corepack pnpm run check` 和 `corepack pnpm run test:run`。

## 兼容与回滚

这是测试版本的权限配置契约收敛。含冒号 operation key 的 `config.toml` 按现有 loader
规则备份并以默认安全配置启动，不做隐式迁移。回滚源码前应恢复旧版本的配置备份，避免
新旧 capability 语法混用；数据库无需重置。
