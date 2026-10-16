# ADR 1261：通过认证 broker pipe 报告执行 gate 状态

## 状态

已实现。broker CLI 现在分别显示 SCM service state 和 startup quota gate；真实 HCS/镜像兼容性与超额写入验收仍是独立开放 Gate。

## 背景

ADR 1260 允许 broker 在启动 quota probe 失败后继续运行协议 listener，使 `Execute` 能在接收 workspace 前返回 `quota_unattested`。因此 SCM `SERVICE_RUNNING` 只表示 service 进程和 listener 生命周期仍可用，不表示执行 gate 已开放。原 `status` 子命令只查询 SCM，用户无法区分这两种状态。

## 决定

- broker v5 协议增加一次性 `ExecutionGateStatusRequest` / `ExecutionGateStatus` 查询，状态只有 `quota_attested` 与 `quota_unattested`。查询必须先通过现有 Hello 协商和 named-pipe peer 身份校验，不接受路径、执行参数或 caller 自报身份。
- 响应后由客户端发送 `ExecutionGateStatusAcknowledged`。pipe 是一次请求一连接；ACK 确保服务端不会在客户端读取前主动断开该 pipe 实例。
- 服务端只根据进程内 `QuotaRuntimeAttestation` 的存在状态生成结果。响应不携带 HCS 原始错误、路径、系统详情或探针输出，避免把内部错误内容带入 IPC。
- `haven-container-broker status` 同时打印 SCM 状态和 execution gate 状态。服务不运行时报告 gate closed；认证状态查询失败时报告 unknown，不把 SCM Running 推断为 quota attested。
- 状态查询不创建 HCS guest、不重新运行 quota probe，也不改变 `Execute` 的 fail-closed 顺序。真实 HCS 兼容性、产品镜像和许可仍不能由此状态替代。
- 协议版本升为 v5；旧版本协商失败，不保留 v4 双向兼容分支。更新后必须由新服务进程提供该状态查询。

## 替代方案

- 继续只显示 SCM `Running`：会把服务存活误报成执行能力可用，拒绝。
- 在状态响应中透传 `io::Error` 或 HCS 文本：可能泄露宿主路径和系统细节，拒绝。
- 让 status 命令创建 guest 重新验收：状态查询会产生写入和启动开销，也会并行化服务唯一的 quota proof 生命周期，拒绝。

## 影响与迁移

无数据库、用户配置或持久数据变化，无需重置。内部 broker wire 从 v4 升至 v5；运行旧协议的服务不会响应新版本查询，CLI 显示状态未知，执行请求继续由协议版本协商 fail closed。新版本 service restart 后提供状态查询。

## 验证

- 自动测试覆盖状态请求/响应编解码、两种 gate 状态、认证 named-pipe 查询、响应 ACK 和无 proof 时报告 `quota_unattested`。
- 已通过 `cargo fmt --all -- --check`、`cargo test --locked -p haven-platform` 与 `cargo clippy --locked -p haven-platform -- -D warnings`；workspace 编译用于验证 broker CLI 调用点。
- 未运行真实 HCS guest、quota overrun 或 Windows 服务升级验收；状态查询只报告本进程 probe proof，不声称目标系统/镜像的产品兼容性验收通过。

## 回滚

移除 v5 状态消息、CLI 查询和 service handler，恢复只打印 SCM 状态；若回退 service 与 client wire，双方需一起回到同一协议版本。不得将 `SERVICE_RUNNING` 重新解释为 runtime quota 已验证。
