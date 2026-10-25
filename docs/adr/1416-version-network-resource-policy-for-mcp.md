# ADR 1416：用统一网络决议和版本化快照连接 Tools 与 MCP

## 状态

已采纳。

## 背景

ADR 1414 让 Tools 授权运行态以 `NetworkResourcePolicy` 同时持有访问模式和域名范围，但硬边界仍由两个字符串 helper 分别检查。拒绝联网、opaque 目标、域名不符和本地地址最终都折叠成通用网络策略错误，授权结果也没有机器可读的网络结论。MCP manager 另持有一个 `NetworkPolicy` 标量，协调器从传入配置再次映射并更新它，MCP 因而拿不到域名范围或策略版本。

## 决定

- Tools 授权 owner 统一解析网络边界，并在 `AuthorizationCheck` 返回 typed `NetworkPolicyDecision`：`DeniedNetworking`、`OpaqueTarget`、`DomainMismatch`、`LocalAddress`、`RequiresConfirmation` 或 `Allowed`。详细用户文案仍由授权决议提供；网络结论不替代完整的权限、风险和审批判断。
- `AuthorizationEngine` 为全局 `NetworkResourcePolicy` 维护单调进程内 revision，仅在访问模式或域名范围改变时递增。协调器从授权 owner 捕获 `NetworkResourcePolicySnapshot` 并将同一快照交给 MCP manager；MCP 忽略旧 revision，并拒绝同 revision 不同内容的冲突快照。
- MCP HTTP 使用快照中的访问模式与 `allowed_domains` 验证目标。`Restricted` 或带域名范围的 `Open` 使用公共地址解析、固定 DNS 结果并禁用代理；不带域名范围的 `Open` 保持现有行为。
- MCP stdio 的网络目标不可检查：`Restricted` 或存在域名范围时拒绝启动。MCP manager 继续独立拥有连接、重连和取消生命周期；此决定不合并各入口执行器。
- 策略值变化会移除由旧策略创建的 MCP client generations，并要求显式重新发现。只更新 revision 且策略内容相同不会断开客户端。

## 影响与迁移

只改变进程内授权结果与 MCP manager 的策略输入，不改配置、IPC、数据库或持久化格式，无需重置数据。HTTP MCP 连接现在会遵守全局和工具配置传来的域名范围；受限网络下的 stdio 继续拒绝，Open 且有域名范围时也拒绝无法执行该范围的 stdio。

## 验证

- 保留授权 hard-boundary 顺序和 MCP 独立连接生命周期；现有网络授权与 MCP 连接用例继续覆盖拒绝路径。
- 执行 `cargo check --workspace --all-targets --locked`、`cargo clippy --workspace --locked -- -D warnings` 与 `cargo fmt --all -- --check`；本次未执行测试。

## 回滚

移除 typed network decision 和快照 revision，恢复 coordinator 向 MCP 传入 `NetworkPolicy` 标量，并让 MCP HTTP 仅按访问模式校验。无持久数据需要回滚；回滚会重新允许 MCP HTTP 忽略域名范围。
