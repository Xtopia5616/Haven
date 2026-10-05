# ADR 0495：MCP Connect 网络边界归入操作契约

## 状态

已采纳并实施（2026-10-05）。

## 背景

原生 UI 路径通过 `AdminRequest::network_access()` 将 MCP 管理操作标为 `Opaque`。模型可见的 `haven.mcp.mcp_connect` 则由 `OperationContract` 构造 `OperationSpec`；通用属性推断未识别该 capability，最终给出 `NetworkAccess::None`。授权引擎在 `NetworkPolicy::Deny` 下只会阻止非 `None` 网络操作，因此模型路径可能越过网络拒绝边界，而原生路径会被阻止。

该差异可由真实 policy projection 复现：测试从操作契约构造 MCP Connect policy 后，`None` 不触发 deny 分支。`mcp_connect` 每次执行都会尝试连接，因此语义应与现有原生 typed request 的 `Opaque` 分类一致。

## 决定

1. 将 `haven.mcp.mcp_connect` 的 `NetworkAccess::Opaque` 显式记录在 `OperationContract`，使 model-facing `OperationSpec` 和 native `AdminRequest` 从同一声明取得分类。
2. native MCP 管理请求对未在 contract 声明网络 override 的现有操作继续保留既有 `Opaque` 分类；本次只统一 `mcp_connect`，不推断配置变更、重载或 refresh 操作的网络行为。
3. 用授权回归测试确认该操作在 `NetworkPolicy::Deny` 下以 `NetworkPolicy` 原因码阻止；同时验证原生请求与模型 operation view 的网络分类相同。

## 替代方案

- 在授权引擎中按 `haven.mcp.*` 名称推断网络访问：拒绝。它会使策略不再由 operation contract 声明，并可能把不发起网络的管理操作一并阻止。
- 只在 App 原生命令中保留 `Opaque`：拒绝。模型可见 `OperationViewTool` 有独立的正式策略对象，App 修复不会改变该调用路径。
- 将所有 MCP 管理操作统一标成 `Opaque`：拒绝。`mcp_add`、`mcp_update` 与 refresh 的外部副作用取决于参数或运行状态，需逐项建立证据。

## 影响与验证

模型和原生 `mcp_connect` 的策略一致，`Deny` 与 `Restricted` 不会把不可检查的网络目标当作无网络操作。Ask/Open 下的确认与执行流程不变；其它 MCP 管理 operation、permission key、IPC、schema、配置和数据库均不变。

回归测试先在旧实现上失败，观察到模型 policy 为 `NetworkAccess::None`；加入 contract override 后，`cargo test --locked -p haven-tools mcp_connect` 通过。完整 workspace 门禁结果随本切片提交记录。

## 回滚

删除 `haven.mcp.mcp_connect` 的 contract override，并恢复 native request 的既有直接分类即可；无需数据库、配置或用户数据重置。若未来 `mcp_connect` 不再尝试网络，应更新该 contract 与 deny 回归测试，而不是改由授权引擎按名称推断。
