# ADR 1263：在授权前绑定 Deferred MCP 配置与 transport

## 状态

已采纳并实施。

## 背景

ADR 1262 将执行边界与网络边界拆开后，配置 transport 已知的 HTTP MCP adapter 可以在 `WorkspaceWrite` 下作为 `InProcess` 能力参与授权；stdio adapter 仍是 `HostProcess`。Deferred `load_mcp` 却在实际调用时才读取 server config，授权器只能保守地把它当作可能启动 stdio 的 `HostProcess`，因此 HTTP Deferred 也被拒绝。

授权前若只读取 transport、执行时再单独读取配置，还会产生不一致窗口：授权检查的目标和真正连接的 server 可能来自不同配置版本。按 server 名查找已经存在的 client 也不够，因为配置正在应用时，旧 stdio generation 可能仍暂时存活。

## 决定

- 新增统一的 `ToolAuthorizationContract`，由工具在一次解析中返回 operation policy 与可选目标绑定。授权请求解析、CodeMode 与 Workflow 的子调用都使用这份契约，避免分别读取可变策略和目标身份。
- `load_mcp` 在授权前从可信 `ConfigService` 的单个不可变 snapshot 解析 server。缺失或禁用的 server 在授权解析阶段拒绝；transport 不接受模型输入覆盖。
- 目标绑定包含配置版本、server 名和 transport，不含 URL、凭据或其它秘密。当前使用 `ConfigService` 全局版本，因此任何配置更新都可能保守地使待执行的授权目标失效。
- HTTP config 将 `load_mcp` 标为 `InProcess`，仍保持 `NetworkAccess::Opaque`，继续受 `Ask` / `Restricted` / `Deny` 与现有确认规则控制。stdio config 将其标为 `HostProcess`，在 `WorkspaceWrite` 下于 spawn 前拒绝。
- Tools 执行入口在连接前重新读取配置并要求目标绑定完全匹配，然后只使用该 snapshot 执行。若已有 client generation 与配置不一致，或发现旧 client 的 transport 与 adapter 的可信 transport 不同，则 fail closed；不能把旧 stdio generation 重新解释为 HTTP。
- MCP adapter 在授权解析和每次执行时都检查配置 transport 与 live client generation；执行继续使用既有 target-bound MCP 调用路径。

## 替代方案

- 保持所有 `load_mcp` 都是 `HostProcess`：安全但持续阻断配置为 HTTP 的 Deferred MCP，与 ADR 1262 的边界拆分不一致。
- 只在授权时读取 transport，执行时仍按 server 名复用任意 live client：拒绝，配置更新/重连窗口可让已授权的 transport 与实际 generation 不一致。
- 让模型声明 transport 或 endpoint：拒绝，模型输入不是权限或连接目标的权威来源。
- 对配置绑定只记录 server 名：拒绝，同一名称的 transport 或其它连接配置可能已经改变。

## 影响与回滚

在 `WorkspaceWrite` 下，Deferred HTTP `load_mcp` 可在普通授权和网络确认完成后连接可信配置目标；Deferred stdio 仍被拒绝。配置更改可能令一次尚未执行的授权失效，需要重新发起。`ToolAuthorizationContract` 仅为运行时授权契约，不改变持久配置、数据库 schema 或 renderer wire payload，无需数据重置。回滚本 ADR 的代码会恢复 `load_mcp` 的保守 HostProcess 拒绝；已保存的 server 配置无需迁移。

## 验证

- 通过实际 `ToolsFacade` 授权解析确认 HTTP 与 stdio 从可信 config 得到不同 execution boundary，且策略解析不创建 client。
- 确认配置在授权和执行之间改变时，旧 target binding 在连接/启动前失效。
- 以 live HTTP MCP server 验证旧 client generation 与新配置不一致时拒绝复用；以 manager 测试验证 transport 不匹配时不给出 client target binding。
- `cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`、ADR 索引和 crate 依赖门禁均按本切片执行；本轮不运行真实 HCS 或 guest 超额写入验收。
