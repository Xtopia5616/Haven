# ADR 1240：将 workspace 快照接入 broker named-pipe

## 状态

已实施 v2 Execute/ready/terminal client 流、带单一总 deadline 的 workspace sender，以及 service 侧 staging receiver/清理路径，并通过本地 named-pipe 端到端传输测试。service 的 quota gate 当前固定返回 `quota_unattested`，所以生产请求会在 staging 分配与 workspace 传输前 fail closed。HCS provider、guest 执行与输出 review 尚未连接；按用户要求不安排 HCS 实机验收。

## 背景

ADR 1238 提供与具体 transport 无关的有界 sender/receiver，ADR 1239 提供由 service 持有的 attempt staging。named-pipe 目前只消费单个 Execute 并回复 quota 状态；它尚未定义何时 client 可开始发送 workspace，也没有把接收端接到 attempt capability。高权限 service 必须先校验 request 与 quota gate，再明确请求快照，不能在未 attested 时预先接收用户数据。

## 决定

- 协议增加带 request ID 的 `WorkspaceSnapshotRequested`。client 在新认证 pipe 上发送 Execute，随后只接受同 request ID 的 quota/backend Failure，或同 request ID 的 snapshot request。遇到失败时不发送 workspace；错误或不匹配的响应会关闭该次连接。
- sender 在收到匹配的 snapshot request 后，使用当前 request 的 workspace limits 流式传输 capability-pinned source；单个 execution deadline 覆盖 Execute、等待授权信号、workspace entries/chunks 和最终响应。取消 event 会中断发送；连接对象被消费，避免部分传输后复用。
- service 对 Execute 重新校验 request 和 typed limits，再调用唯一 `quota_runtime_attestation` gate。gate 未通过时立即返回带 request ID 的 `QuotaUnattested`，不创建 attempt、不读取 workspace frame，也不写入 scratch root。
- quota gate 将来通过后，service 创建单个 `BrokerWorkspaceAttempt`，回复 `WorkspaceSnapshotRequested`，再用 `WorkspaceSnapshotReceiver` 将 entry/chunk 流写入该 attempt 的 pinned staging capability。receiver 失败时先尝试删除整个 attempt，再返回不含原始 OS/path 错误的 typed failure；清理失败返回 `CleanupFailed`。完整传输后当前尚无 HCS provider，因此丢弃 staging 并返回 `BackendUnavailable`。
- IPC 协议升至版本 2，旧版本不能协商成功。当前 broker 尚未进入正式发布，按项目破坏性重构约定不保留 v1 兼容分支。协议仍不携带 host root、授权 proof、session identity、HCS JSON 或 executable path。
- 此切片不启用 HCS，也不消费 stdout/stderr 或 output manifest。quota runtime attestation 与 guest 超额写入仍控制生产执行；QuotaUnattested 时服务不会为任何 client 接收文件。

## 替代方案

- client 发送 Execute 后不等 service 响应就发送 4 GiB workspace：拒绝，quota gate 拒绝时会造成不必要的特权磁盘写入。
- service 在 quota gate 前创建 attempt 或读 snapshot：拒绝，未证明 HCS 硬配额时不能因控制面请求消耗 broker scratch。
- 对接收端错误后保留部分文件供重试：拒绝，部分 snapshot 不能成为下一次执行的输入；owner 必须清理整个 attempt。
- 保持协议 v1 并让旧 binary 忽略新状态：拒绝，独立 broker 与主程序必须对执行阶段语义达成一致。

## 影响与迁移

增加一个关联 request ID 的控制消息、client 单次执行传输方法，以及 service staging receiver 调用；named-pipe 版本更新为 2。没有数据库、用户配置或 Tauri IPC schema 变化，不需数据重置。已安装旧版测试 broker 需与新主程序一起重建/替换；双方握手会拒绝版本不一致。

## 验证

- `cargo fmt --package haven-platform -- --check`
- `cargo check --locked -p haven-platform`
- `cargo clippy --locked -p haven-platform -- -D warnings`
- `cargo test --locked -p haven-platform`
- 本地命名管道测试走过 Execute、关联 ready、workspace 文件和 chunk、snapshot complete、terminal failure 全链，并校验接收字节；协议测试覆盖 ready request ID 与 v1 拒绝；取消测试确保 receiver 在读取前停止。
- quota gate 单测确认未 attested 时返回 `QuotaUnattested`。由于该 gate 固定 fail closed，本地端到端 pipe 测试验证的是独立 transport path，不是 SCM service 的 quota 成功路径。
- 未执行安装服务、HCS compute system、guest 超额写入、工具 Shell/Tools 端到端或正式发布升级测试。

## 回滚

回滚到 ADR 1239 的 scratch owner 状态需移除 v2 消息、client sender/receive orchestrator，并恢复 service 对 Execute 直接返回 `QuotaUnattested`。回滚后主程序和 broker 必须同时重建；不得保留协议 v1/v2 的自动 fallback。运行目录不因回滚自动删除。
