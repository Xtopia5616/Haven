# ADR 1259：将 WorkspaceWrite Shell 接入 HCS broker

## 状态

实现已接入 Tools Shell 与现有 review owner。broker quota runtime attestation 仍 fail closed；本 ADR 不代表真实 HCS 执行或超额写入验收已通过。

## 背景

ADR 1258 已提供 Windows sealed execution provider，但 Tools Shell 尚未消费它。直接保留 host process 会让 `WorkspaceWrite` 下获准的 Shell 逃出 workspace 边界；长时间持有现有 `ExecutionPolicyPermit` 又会阻塞策略更新，不能及时撤销运行中的 guest。

## 决定

- Windows `WorkspaceWrite` 的前台 Shell 只经 broker provider 执行，允许固定 guest 中的 `cmd` 和 `powershell`。`pwsh`、后台 Shell、缺少 live execution authorization、缺少 canonical session/step id 或缺少已授权 workspace 时均 fail closed。Shell 不回退到宿主进程。
- `FullAccess` 保持既有本机 Shell 启动和进程撤销路径；`ReadOnly` 拒绝 Shell。非 Windows 不开启 WorkspaceWrite 的 opaque Shell 例外。Skills、MCP 和其它 opaque capability 仍被 sandbox gateway 拒绝；`Deny` 与 `Restricted` 网络策略保持原规则。HCS 配置不含网络端点。
- Shell 输入只使用 proof 绑定的 `ExecutionWorkspace` capability。cwd 转成 workspace-relative 路径；workspace root 表示为空路径，外部绝对路径和 `..` 均拒绝。Host root 不跨 broker IPC。
- 每次运行最多复制 128 MiB workspace、单文件最多 64 MiB，最多 10,000 项和 32 层；输出 diff 最多 128 MiB。HCS 请求声明 512 MiB scratch、300 秒超时；stdout/stderr 各受 `collect_byte_cap(max_chars)` 与 64 MiB 双重上限约束。
- 执行前，Tools review owner 预留 384 MiB。该值覆盖 `2 × 128 MiB baseline + 128 MiB changed output` 的保守 retained storage 上限，并留在既有 512 MiB review owner 总预算内。无论 Shell 成功或返回非零 exit code，只要完成了 guest 执行，输出 review 都交给同一个 owner；取消或失败会释放 reservation。
- broker wire 升至 v4，并在 seed 完成后加入三帧启动握手：broker 请求创建 Shell；Tools 在请求内取得短期 `ExecutionPolicyPermit` 并授权；broker 创建 HCS Shell 进程后确认启动，Tools 随即释放 permit。由此 permit 只保护进程创建边界，不跨越整段执行。运行期间 policy/tool/session 撤销会取消 pipe，Tools 等待 broker worker 完成 guest 停止与清理后再返回。
- quota gate 仍位于 attempt 分配和 workspace 接收之前。`quota_unattested` 会作为 permission failure 返回，不读 workspace、不创建 guest，也不会 host fallback。只有之后实现并验证 runtime quota attestation，才可打开执行。

## 替代方案

- 在 WorkspaceWrite 下继续启动 host Shell：拒绝，会绕过 workspace 隔离。
- 让 Shell 持有策略读锁直到命令完成：拒绝，会阻塞安全策略更新，无法及时撤销 guest。
- 仅在启动前检查 proof、启动后不监控撤销：拒绝，活动 guest 会在撤权后继续运行。
- 把 broker 输出直接写回 workspace：拒绝，用户仍需通过既有 review 与单文件晋升授权链。

## 影响与迁移

无数据库、持久配置或 UI/IPC schema 变化，无需重置。wire 与 named-pipe 名称升至 v4；旧服务不匹配时 provider 连接失败并 fail closed。FullAccess 用户路径不变。`WorkspaceWrite` 前台 Shell 只有在 broker 可用且未来 quota gate 开启后才能运行；当前仍会明确报告 quota 未验证。

## 验证

- 自动测试覆盖启动握手 request id、客户端只在正确顺序调用 permit/started callback、quota gate 拒绝时不进入启动回调、HCS guest permit 精确包围 Shell process 创建、WorkspaceWrite cwd 限定、Skills 仍被拒绝。
- 已通过 `cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`、UI `check` / `test:run` / `build`、ADR index、crate dependency inventory 与 `git diff --check`。
- 未运行真实 HCS guest、broker quota attestation 或 guest 超额写入验收；按用户要求暂不安排验收环境。

## 回滚

撤销本 ADR 涉及的 Tools Shell/provider 接线、Windows Shell sandbox gate 例外、HCS 启动 permit 握手与 v4 wire 变更，并恢复 broker v3 名称/协议。回滚后 WorkspaceWrite opaque Shell 必须继续拒绝，不能恢复 host fallback。
