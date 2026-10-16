# ADR 1258：实现 Windows broker execution provider

## 状态

平台已提供 sealed `WindowsContainerExecutionPort` 的 Windows broker 实现与工厂。尚未接入 Tools Shell；service quota gate 仍拒绝所有执行请求。未运行真实 HCS 或 guest 超额写入验收。

## 背景

ADR 1257 已将 broker service 的 HCS attempt、输出导出与 v3 terminal ACK 接通。平台仍缺少从 typed execution port 到 authenticated named-pipe client 的生产 provider，因此 Tools 不能将 bounded request、workspace capability、实时 stdout/stderr 和 `SandboxOutputReview` 交给 Windows broker。

## 决定

- 在 `haven-platform` 内实现 sealed `WindowsContainerExecutionPort` provider，并暴露窄工厂返回 `Arc<dyn WindowsContainerExecutionPort>`。实现留在平台 crate；调用方只能消费 port，不能提供任意 provider 或直接操作 named-pipe 实现。
- 每次执行都从当前 Windows token 创建 `BrokerClientPolicy`，并创建新认证 pipe。请求 correlation 使用每次调用的随机 128 位 UUID bytes；它只存在于 v3 pipe 生命周期，不写入 durable store、transcript、配置或日志，不是业务实体 ID。
- provider 通过进程级单槽拒绝并发 attempt。broker listener 与 attempt scratch owner 当前都只支持一个活动请求；其它调用立即以 `WouldBlock` 失败，不在未计入 request deadline 的队列中等待。
- provider 复用 typed request、隐藏 workspace root 的 `WindowsContainerWorkspaceInput`、有界 stdout/stderr sink 与 client output review reconstruction。成功只返回 `WindowsContainerExecutionResult`；review 仍由后续 Tools owner 预留并持有，不在 provider 中晋升任何文件。
- caller cancellation 需要能在执行期间与 I/O 同步发生。`is_cancelled` callback 增加 `Send` 契约；每个执行创建一个 scoped watcher，每 20 ms 检查调用方状态。取消时设置 pipe I/O event，打断 client read/write 并关闭该连接；broker 将 peer disconnect 传播到 guest cancellation、显式停止与 attempt cleanup。watcher 在 operation 返回或 panic 展开时退出。
- broker failure code 与 pipe/transfer 错误都映射成静态 `io::Error` kind/message，不透传原始 Windows OS text、命令、路径或 provider detail。`QuotaUnattested` 映射为 `PermissionDenied`；不能把 service 正在运行解释为执行 backend ready。
- 当前工厂不改变 Tools 可达性或 service gate；接入 Tools 后仍须先预留已有 pending-review 容量，并且只有未来独立完成 quota runtime attestation 后才能考虑打开生产执行。

## 替代方案

- 让 Tools 自行创建 named pipe、拼协议和校验 broker 身份：拒绝；这会把平台认证与传输细节泄漏到能力 owner。
- 在同步 I/O 返回后才检查 cancellation：拒绝；无输出的长 Shell 期间无法及时中止 guest。
- 在 client provider 内直接写回 workspace：拒绝；输出必须继续由 Tools review、授权和 capability 晋升链处理。
- 把原始连接错误返回给用户：拒绝；系统错误可能暴露宿主路径或服务实现细节。

## 影响与迁移

增加 Windows-only provider 工厂、取消 watcher 与 `uuid` 直接依赖；UUID 只用作未持久化的协议关联 nonce。没有数据库、用户配置或 broker wire 变化，无需重置。sealed execution port 的 cancellation callback 现在要求 `Send`，已有内部 mock 与未来消费方需满足此边界。provider 尚未从 Tools 调用，运行时行为保持不变。

## 验证

- `cargo fmt --all --check`
- `cargo test --locked -p haven-platform`：156 passed、0 failed、1 ignored；ignored 项需要 elevated HCS storage-formatting privilege。
- `cargo clippy --locked -p haven-platform -- -D warnings`
- ADR index checker。
- 单测覆盖 caller cancellation 会设置 pipe cancellation event、provider 单槽并发上限、安全 transport error 映射，以及 `QuotaUnattested` 保持 permission denied。
- 未连接真实 SCM broker service、HCS guest 或 quota runtime attestation。

## 回滚

删除 broker provider、工厂和 `uuid` 直接依赖，并将 cancellation callback 签名恢复为原边界。broker service v3 与 ADR 1257 保持不变；生产执行仍必须在 quota gate fail closed，不能改走普通 Shell 当作替代 backend。
