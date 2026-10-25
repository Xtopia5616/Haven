# ADR 1402：域名限制配置下拒绝 Shell 联网执行

## 状态

已实施。

## 背景

全局和工具级 `ResourcePolicy.network.allowed_domains` 是硬域名边界。HTTP 工具声明精确 `NetworkDestination`，授权网关及 HTTP 执行器可以验证目标；Shell 的网络目的地由任意子进程决定，MXC 请求目前只携带 `allow_network`，无法把已配置的域名列表落实到子进程的连接上。普通联网确认因此只能确认开放网络访问，不能把 Shell 限定到允许的域名。

## 决定

1. 若全局或 Shell 工具级网络策略配置了一个或多个允许域名，授权网关拒绝 `shell` 的 opaque 网络能力，不发起普通联网确认，也不允许后续执行。拒绝在副作用前进行，并覆盖 `WorkspaceWrite` 的 MXC 路径与 `FullAccess` 宿主路径。
2. 网络访问已由既有 `Deny` 或 `Restricted` 策略拒绝时，沿用原有拒绝路径。没有配置域名列表时，现有 Shell 联网授权流程不变。
3. HTTP 的 typed destination 校验与一次性域名边界确认不变。此决定不声称通过检查 Shell 参数、命令文本、DNS 或 UI 对话框来实现域名隔离。
4. 将来若启用 Shell 域名限制，必须先限制子进程直接出站，再让受控代理按同一域名策略过滤目标；在此能力可用并经验证前，Shell 保持硬拒绝。

## 配置、数据与兼容影响

没有配置 schema、数据库、session event、IPC 或重置变化。用户现有域名列表继续对 HTTP 生效；在存在全局或 Shell 工具级列表且网络策略允许联网时，Shell 会新增明确拒绝。

## 替代方案

- 把 MXC 的 `allow_network=true` 当成域名隔离：拒绝，该开关只控制网络整体，不能限定域名。
- 扫描命令字符串推断目标主机：拒绝，Shell 语法、脚本、DNS 和子进程使其无法构成可靠边界。
- 仅显示普通联网确认：拒绝，确认不会修改 MXC 的网络能力，无法兑现配置的硬限制。

## 验收

- 安全授权回归测试验证：域名列表分别来自全局策略和 Shell 工具级策略时，未声明网络目的地的 Shell 在联网确认前被 `NetworkPolicy` 拒绝。
- 对照测试验证：开放网络且没有域名列表时，Shell 仍走既有确认流程。
- `cargo fmt --all -- --check` 通过。
- `cargo test --locked -p haven-tools --lib shell_with_domain_restrictions_is_blocked_before_network_confirmation` 通过。
- `cargo test --workspace --locked`、`cargo check --workspace --locked` 与 `cargo clippy --workspace --locked -- -D warnings` 通过。
- 安全设置回归测试、Svelte 检查、完整 UI 测试与生产构建通过。
- 此变更不依赖 MXC Windows 运行时路径，因为拒绝发生在 MXC spawn 前。
