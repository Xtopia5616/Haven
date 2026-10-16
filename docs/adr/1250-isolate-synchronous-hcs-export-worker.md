# ADR 1250：隔离同步 HCS writable-layer 导出

## 状态

已实现私有单进程 broker helper、有限请求协议、服务身份校验与 attempt 保留语义；没有接入 broker 请求路径，也未调用真实 HCS/ComputeStorage API。此 worker 只界定时间与进程，不提供 host transport 目录的硬磁盘配额；quota gate 继续 fail closed。

## 背景

ADR 1249 的 `HcsExportLayer` 是同步 native 调用，没有取消或 deadline 参数。在线程中调用后再返回超时，会留下仍可读写 layer 和 transport staging 的执行；直接删除 attempt 会造成资源竞态。[Microsoft API signature](https://learn.microsoft.com/en-us/virtualization/api/hcs/reference/hcsexportlayer) 仅暴露 layer、export folder、layer data 与 options 参数，没有 operation/cancellation handle。

## 决定

- broker 使用当前受保护的 `haven-container-broker.exe` 启动隐藏 helper mode。helper 在读取请求前验证 session 0 与固定 restricted broker service SID；交互用户令牌不能调用该路径。helper stdin 只接受一个 version 1 长度帧，JSON 最大 256 KiB，拒绝尾随数据、未知字段、过量父层、无效本地绝对路径、无效 VHDX、非空 transport 目录和互相重叠的层目录。`IsWritableLayer` 在 helper 内固定为 `true`。
- 父进程从已验证 `HcsLayerExportPlan` 生成请求，不接受模型或 broker client 提供的 host path/HCS JSON。父层目录与 writable layer 在调用期间由 capability pin；helper 重新打开并验证目录后才加载 System32 中的 `computestorage.dll`。
- helper 在创建时保持挂起，先分配给 kill-on-close Job Object，再恢复初始线程。Job 最多允许一个活动进程，阻止 helper 创建未受管后代。执行 deadline 使用请求 timeout 且不超过现有工具上限；取消、超时、等待错误会终止 Job 并在有界期限内等待进程树归零，无法确认归零时保留 attempt。
- helper 正常返回（HCS API 已返回 HRESULT）且 Job 确认为空后，调用方才能清理 attempt。timeout/cancel 触发强制终止时，即使本地进程树已归零，也保留 attempt：API 未定义 caller 被终止后可能存在的 host-side 写入。此类 attempt 将非空 quarantine 状态写入 owner marker；新 attempt 因 scratch root 非空而失败关闭，stale sweeper 永不自动清理该 marker。必须先确认 HCS 不再使用 staging，再由管理员手动移除 attempt。
- `HcsExportLayer` 没有输出字节上限。decoder 的文件树上限发生在导出完成之后，不能保护 host staging 卷免于导出过程写满；在接入 provider 前，仍须为 transport staging 建立经 OS 强制的容量边界或证明另一种等价硬上限。
- Microsoft API 文档没有定义 caller process 被终止后 export 是否仍会在系统服务侧写入；目标 Windows build 的 helper 被强制终止后的 transport 生命周期也须在生产接线前证明。未得到该证据时，不得把“helper process tree 已空”提升为可清理/可复用 staging 的 HCS 运行期证明。quarantined attempt 不参与自动回收。
- helper 成功只表示 HCS export API 返回成功。父进程仍须用 ADR 1248 的 decoder 校验 transport 并生成 output diff/review；任何失败都不能晋升到 canonical workspace。
- quota runtime attestation 与真实 guest 超额写入仍是 `WorkspaceWrite` backend 的硬 gate。当前 service 请求仍在分配 attempt 和读取 workspace bytes 之前返回 `quota_unattested`；本 ADR 不改变 gate。

## 替代方案

- 在线程中运行并在 timeout 后放弃 join：拒绝；同步 HCS 调用仍能访问 attempt，无法证明清理安全。
- 在 service 进程内调用 HCS 后轮询取消：拒绝；native API 没有取消参数，SCM stop 或 pipe 断开都不能中断调用。
- 超时后立即删除 attempt：拒绝；必须先观察到 Job 中无活动进程，并确认 helper 退出。

## 影响与迁移

新增 broker 私有 CLI mode、执行模块和 `BrokerWorkspaceAttempt::preserve_for_recovery`。未改变用户配置、数据库、MCP/工具授权或 broker wire protocol，不需要重置。未连接 provider/service，因此现有执行行为不变。超时/取消造成的 quarantine 没有自动清理命令；管理员确认系统不再访问 attempt 后手动清理。若本机无法创建 Job Object、helper 身份不匹配、请求验证失败或 cleanup 状态不确定，执行保持关闭。

## 验证

- `cargo fmt --package haven-platform --package haven-container-broker -- --check`、`cargo check --locked -p haven-platform` 与 `cargo check --locked -p haven-container-broker` 通过。
- 平台测试覆盖 worker request 的严格 framing/version/大小、validated plan 往返、重叠 layer 拒绝、attempt 在未确认进程结束时保留并阻塞新 attempt；worker failure 明确表达 cleanup 是否安全。
- 进程树终止使用现有 `ProcessContainment` Windows Job Object 测试契约；本切片没有调用 HCS native API、启动 guest 或进行 quota acceptance。

## 回滚

可移除 helper mode、worker 模块、单进程 Job 构造器和 attempt recovery 保留入口；quota gate 必须继续 fail closed。接入 service/provider 前需重新审查失败路径是否均使用 `cleanup_safe` 决定 attempt 清理。
