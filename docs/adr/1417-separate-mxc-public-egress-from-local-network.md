# ADR 1417：分离 MXC 公网出站与本地网络权限

## 状态

已实施。

## 背景

Haven 的 MXC ProcessContainer adapter 过去把 Shell 的 `allow_network` 映射为公网出站、私网入站和宿主 loopback 三项都允许。MXC 的网络模型将公网 egress、私网 `privateNetworkClientServer` 与宿主 loopback 分开；私网能力还是双向的。单一布尔值因此扩大了实际权限，也让“Shell 联网”含义不清。

## 决定

- `SandboxedProcessRequest` 使用 typed `SandboxedProcessNetworkPolicy`，分别表达 `allow_public_egress`、`allow_private_network_client_server` 和 `allow_host_loopback`。
- 兼容隔离策略只可收窄权限：公网出站兼容档保留已请求的公网出站，私网和宿主 loopback 强制关闭；断网档三项全部关闭。
- Shell 的 Open 网络授权只请求公网出站；Restricted 与 Deny 不请求公网出站；Shell 不自动请求私网或宿主 loopback。
- MXC 原生进程网络规则不支持域名 allowlist、端口或协议范围。存在域名约束时，Shell 继续在授权层拒绝网络访问，直到有受控、可探测的策略执行器。
- 隔离报告记录三项实际权限，UI 提示分别描述它们；公网开放不再被表达为私网或本机服务开放。

## 替代方案

- 保留布尔值并让 Open 同时开放公网、私网和 loopback：拒绝，因为它把互不等价的网络边界合并并授予过宽权限。
- 通过环境代理实现 MXC 域名约束：拒绝作为本 ADR 的实现，因为子进程可绕过代理变量，不能证明完整 egress enforcement。
- 为了兼容旧报告继续声称 `allow_network`：拒绝；这是进程内请求和报告契约，不涉及持久配置。

## 影响与迁移

MXC request、隔离报告和 Shell 设置说明的进程内契约发生变化。配置、IPC、数据库和持久化格式不变，无需重置。Shell 的 Open 继续允许公网出站，但不再因此允许宿主 loopback 或私网；需要访问本地 provider 的调用目前仍被拒绝，需后续提供明确目标能力。

## 验证

- 平台单元测试检查公网出站不推导私网或 loopback。
- Windows MXC 集成测试检查 Shell Open 下公网出站被请求，同时私网和宿主 loopback 仍拒绝。
- `cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo fmt --all -- --check`、`corepack pnpm run check`、`corepack pnpm run test:run` 与 `corepack pnpm run build` 通过。
- Windows MXC `workspace_write_shell_keeps_host_loopback_separate_from_public_egress` 真实 host 验收通过；隔离层方向权限语义以 MXC 官方 ProcessContainer 网络模型为准。

## 回滚

恢复 `allow_network` 布尔值会重新合并三种访问能力，并可能向 MXC 申请私网与 loopback。无持久数据需要回滚；若必须回退代码，应同步保留 UI 对 Open 的本地访问说明，避免呈现与实际权限不符。
