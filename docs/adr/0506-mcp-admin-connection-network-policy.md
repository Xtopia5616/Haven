# ADR 0506：MCP 管理操作共享建连网络策略

## 状态

已采纳并实施（2026-10-05）。

## 背景

ADR 0495 将 `haven.mcp.mcp_connect` 的网络分类统一到 `OperationContract`，但其它 MCP 管理操作仍有模型/native 策略漂移。模型 `OperationSpec` 从 `operation_contract` 与 `operation_policy_attributes` 取得分类；`haven.mcp.mcp_add/update/toggle/reload` 没有网络 override，通用属性推断把它们标为 `NetworkAccess::None`。原生 `AdminRequest::Mcp` 对未声明 override 的操作则保守地标为 `Opaque`。

该差异影响 `NetworkPolicy::Restricted`：授权网关会拒绝 `Opaque` 网络请求，但不会拒绝 `None`。MCP manager 另有 Deny 策略下的底层连接拒绝，因此本 ADR 不将问题描述为 Deny 下可建立外网连接；实际缺口是 Restricted 下模型调用能绕过操作授权层的 opaque-network 拦截。

这些操作确实能发起连接：add 在启用且 `auto_connect` 时连接；update/toggle 在启用或连接配置变化时可能重连；reload 会连接启用的服务器。操作视图在调用前发布静态策略，因此参数关闭自动连接也不改变该操作级分类。`mcp_list` 只读取当前状态，`mcp_disconnect` 和 `mcp_remove` 只清理已有客户端，不新建连接。

## 决定

1. 在唯一 `OperationContract` 中将 `mcp_connect`、`mcp_add`、`mcp_update`、`mcp_toggle` 和 `mcp_reload` 明确分类为 `NetworkAccess::Opaque`。
2. 将 `mcp_list`、`mcp_disconnect` 和 `mcp_remove` 明确分类为 `NetworkAccess::None`，避免把本地查询/断连操作误报为建连能力。
3. 模型 `OperationSpec` 与原生 `AdminRequest::Mcp` 都从该分类读取网络策略；保留未知 MCP 操作在 native 路径的 `Opaque` fallback，确保新增操作默认 fail closed。
4. 以操作级保守分类覆盖参数与运行时状态组合，不新增动态授权策略框架。能够建立连接的四个操作在 `Restricted` 下必须于 handler 执行前被阻止；本地三项不受网络门禁误拦。

该决定细化并替代 ADR 0495 对未声明 MCP 操作保留原 native `Opaque` 分类的暂缓边界；`mcp_connect` 决定继续有效。

## 替代方案

- 在授权引擎按 `haven.mcp.*` 名称统一推断：拒绝。它会绕过操作契约 owner，并错误阻止 list/disconnect/remove。
- 仅修复 `mcp_add`：拒绝。update、toggle 与 reload 也有真实连接路径，保留它们的 `None` 会继续让同一边界漂移。
- 按每次调用参数计算动态网络分类：暂不采用。现有 operation view 使用静态 `OperationSpec` 授权契约；动态规则会扩展授权接口和测试面，本切片用可说明的保守操作级声明已满足安全边界。

## 影响与验证

- `NetworkPolicy::Restricted` 下，模型侧 add/update/toggle/reload 与 native 请求共享 `Opaque` 分类，并在执行管理 handler 前被授权网关拒绝。
- `mcp_list/disconnect/remove` 的模型和 native 分类统一为 `None`。Deny 下既有 MCP manager 连接拒绝保持不变。
- 权限 key、风险级别、确认要求、MCP handler 顺序、IPC、配置、数据库与持久数据均不变；无需重置用户数据。
- 回归覆盖 operation contract 分类、Restricted 下的模型策略拒绝以及 native/model 分类一致。`cargo test --locked -p haven-tools`、`cargo clippy --locked -p haven-tools -- -D warnings`、`cargo fmt --all -- --check` 通过。

## 回滚

删除该 ADR 引入的 MCP 网络 override，即可恢复 ADR 0495 之后原有分类；不涉及数据或用户配置回滚。若未来这些管理操作增加新的连接路径，必须更新同一 `OperationContract` 和对应授权回归，不得在 App/native adapter 另建分类表。
