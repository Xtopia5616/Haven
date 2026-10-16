# ADR 1257：将 HCS 执行链接入 broker service

## 状态

已实现服务端执行编排与协议终态确认。quota runtime attestation 仍在 attempt 分配和 workspace 传输前 fail closed；未调用真实 HCS，也未执行 guest 超额写入验收。

## 背景

broker 已能认证客户端、接收有界 workspace 快照，并由客户端把 broker 输出重建为带 workspace generation 与 policy revision 的 `SandboxOutputReview`。HCS compute-system、guest seed/Shell、writable-layer 导出和 bounded output diff sender 也已分别实现，但 service 尚未把这些 owner 串成一次请求。named pipe 过去在写完最后一帧后立即断开，客户端可能尚未读完成功或失败响应。

## 决定

- 将唯一 `quota_runtime_attestation` gate 保持在请求结构校验之后、attempt 分配、snapshot-ready 响应和 workspace 字节读取之前。当前 gate 固定返回 `QuotaUnattested`；因此新增的 HCS 执行链在生产请求上不可达。
- gate 将来通过后，service 才分配 attempt、接收受限 workspace snapshot，并调用 broker execution owner。执行请求仍只包含 typed Shell、相对 cwd、generation/revision 与各项限额；HCS image 由已 pinned 的本地 catalog 选择，HCS system 名从 request ID 派生，不接受客户端 host path 或镜像选择。
- 每次 attempt 在私有目录创建并格式化有界 scratch VHDX，配置固定只读 workspace 输入和 quota-bound writable output；guest 先执行固定 seed，再运行单个 Shell。一个 deadline 覆盖 workspace 接收、guest 生命周期、输出导出和结果发送。取消、SCM 停止、客户端断开或输出写失败会取消 guest；只有显式停止成功才能导出 writable layer。
- 只从已停止且与当前 attempt 绑定的 writable layer 导出。导出继续经过有界 transport VHDX、隔离 helper、固定 workspace-output 解码器和 bounded diff sender。可证明静止的失败清理 attempt；HCS、helper 或 cleanup 状态不确定时保留 scratch/attempt 并 quarantine，禁止自动回收。
- broker stdout/stderr 各自受 request 上限约束；差异流受 output-diff 文件数、深度和总字节上限约束。成功 terminal frame 只在输出传输完成且 attempt 清理成功后发送。
- named-pipe 协议升级到 v3，增加匹配 request ID 的 `ExecutionAcknowledged`。客户端在完整接收并校验 request-scoped failure 或 execution completion 后 ACK；service 收到对应 ACK 才结束本连接，避免在 terminal frame 写入后过早断开。旧协议版本不协商。
- 客户端和服务端的 request-scoped failure 都等待匹配 ACK；无 request ID 的握手/解码错误仍按原方式失败。ACK 不授权重试，也不改变工具授权、确认或输出晋升流程。

## 替代方案

- 只在 workspace 接收后返回 `backend_unavailable`：拒绝；无法形成可审阅的 server-side execution 生命周期，后续容易再次出现未接线分支。
- terminal frame 写入后立即断开：拒绝；客户端会遇到成功/失败帧未送达的竞态。
- quota proof 未完成时暂时放开 gate 做端到端试跑：拒绝；VHDX 容量和配置值不能代替 guest 实际写入超额拒绝的 OS 运行期证明。
- HCS/export 状态不确定时强制删除 attempt：拒绝；guest 或 helper 可能仍持有层句柄，必须保留并隔离恢复状态。

## 影响与迁移

broker named-pipe wire contract 升为 v3，客户端与服务必须同版本协商；不保留 v2 兼容。没有数据库、用户配置或持久缓存变化，无需重置。新增 service owner 连接既有 HCS attempt 与输出 owner，但尚未将它接入 `WindowsContainerExecutionPort` 或 Tools，也不改变生产执行可达性。quota runtime attestation 和支持主机上的 guest 超额写入验收仍是开放 gate。

## 验证

- `cargo fmt --all --check`
- `cargo test --locked -p haven-platform`：152 passed、0 failed、1 ignored；ignored 项需要 elevated HCS storage-formatting privilege。
- `cargo clippy --locked -p haven-platform -- -D warnings`
- ADR index checker。
- named-pipe 集成测试覆盖 request-scoped quota failure ACK、输出接收与 review 晋升、peer disconnect 探测及协议版本边界。
- 未运行真实 broker service/HCS，也未进行 quota runtime attestation 或 guest 超额写入验收。

## 回滚

回滚本切片时移除 service→HCS orchestration 和 v3 acknowledgement，并恢复 broker 协议及 client/service 到 v2。quota gate 必须继续在 attempt 分配和 workspace 读取前拒绝请求；不得以回滚为由开放 HCS provider 或 `WorkspaceWrite` capability。
