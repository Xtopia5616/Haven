# ADR 1285：在 broker service 中监督 MCP generation

## 状态

service 端 open/attach、固定 package HCS 启动、双向 stdio relay、控制状态轮询和已确认停止后的 attempt 清理已实现；mock named-pipe tests 通过。应用侧 MCP launcher 尚未接入此 session client，因此 tool proof/policy revision 的实时撤销也未接到此 owner。真实 Windows HCS/quota 验收按用户要求暂缓。

## 背景

ADR 1279–1284 已定义独立认证 session pipe、固定 HCS package owner、有限输入/输出帧和可轮询的 broker control listener，但 broker 尚未消费 `McpSessionOpen`。没有 service handler 时，managed MCP stdio 在 `WorkspaceWrite` 下仍应 fail closed。

## 决定

- Broker 在控制 pipe 上验证 request、精确的 512 MiB quota attestation、Node runtime 与 catalog 中同 ID 的 package entrypoint proof。验证通过后才 mint 随机 session ID、派生同一 peer policy 的 session listener 并返回 `McpSessionOpenAccepted`；任一门禁失败在 attempt/scratch 分配前拒绝。
- attach 必须通过独立 pipe 的 Hello 和 request/session pair 校验。Broker 随后创建一个私有 attempt 与固定 quota VHDX，不映射 `ExecutionWorkspace`、Skill 输入或调用者路径；HCS 只启动 pinned Node 与 pinned package proof。只有 process launch 成功才发送 `McpSessionReady`。
- HCS owner、quota proof、attempt、scratch 与会话循环留在 broker service 调用线程。会话循环每轮有界轮询 control pipe，答复 execution-gate status，并接受与当前 session ID 匹配的 `McpSessionStop`；活动 session 期间的其他 Execute/Open fail closed。control-plane Stop 可在 guest stdin 背压时取消，不必排在已缓存的数据帧后。没有为 HCS/attestation 引入新的 detached worker。这一 Stop 分支细化了 ADR 1284 的 status-only 轮询计划。
- MCP stdio handler 单独放入 `broker_mcp_session.rs`，避免继续扩展已有职责较多的 SCM/Shell `broker_service.rs`；broker service 保持唯一 listener 入口并把 session 生命周期交给该 owner。
- 文件职责复核：本次后 `broker_service.rs` 为 1270 行，仍持有 SCM 安装/状态与控制回调、唯一认证 listener、既有 Shell 请求处理；这些路径共用同一 service control state 和 listener 生命周期，当前拆分会引入第二个跨模块生命周期 owner。将 MCP generation 专属启动、relay、停止与 attempt 清理隔离到 747 行的 sibling owner 后，暂时保留 service 模块边界。应用侧 session client 接线稳定后，再依据是否出现跨职责回归复核是否把 SCM 管理 API 拆成独立模块；不复制 service state 或 listener authority。
- 输入最多暂存一个 16 KiB MCP frame；HCS writer 背压时暂停读取 session pipe，恢复后原样重试该 frame。EOF 关闭 guest stdin，但仍接收 Stop；stdout/stderr 按独立 16 KiB frame 原样转发。guest 自然退出后最多等待 2 秒排空剩余输出。
- SCM stop 和交互 session change 会停止 guest；session change 完成清理后刷新 peer policy。只有 `HcsMcpGuestSessionOwner::stop` 返回与当前 scratch path 相同的 stop proof，并复核 VHDX maximum size 后，才删除 scratch 并清除 attempt marker；完成这两步后才发送 `McpSessionExited`。自然退出带实际 exit code；客户端主动 Stop 时为 `null`；断连不伪造 Exited。任何无法证明 guest/system 已停止的路径都会 quarantine attempt。
- 将 broker protocol 升至 v10：`McpSessionExited.exit_code` 改为可空，区分自然退出与主动停止；新增 package proof 失败码。该协议尚未发布，不保留 v9 wire fallback。
- 客户端侧 generation launcher 尚未连接 `McpSessionOpen`/session pipe。因此本 ADR 不开放 `WorkspaceWrite` MCP 生产执行，host/local MCP launcher 继续 fail closed。
- Broker wire 当前只携带受管 package ID，不带工具调用 proof 或应用 policy revision；应用接线时必须在创建 generation 前授权，并在 deny/revision 变化时发送 Stop、等候 Exited/stop completion。完成该调用侧撤销协议前，不得开放 `WorkspaceWrite` stdio MCP。

## 替代方案

- 把 HCS API、attestation 或 attempt 移交后台线程以并行处理 status：拒绝，owner 未证明可跨线程移动，且会分裂停止确认与 crash recovery。
- MCP stdout/stderr 走通用 64 KiB chunk 或在本地无限缓存：拒绝，超出单帧/单次 drain 上限并破坏背压。
- 客户端主动 Stop 时伪报 exit code 0：拒绝，强制终止不是进程自然退出，wire 以 `null` 表示无自然退出码。
- 断连后保留正在运行的 guest 供重连：拒绝，generation 与认证 client pipe 一一绑定，断连必须收束 HCS 生命周期。

## 影响与回滚

新增 Windows-only broker service execution path 和 protocol v10 内部消息语义，不改变数据库、持久配置、workspace 或实体 ID，无需重置。回滚时移除 service open/relay handler 并让 `McpSessionOpen` fail closed；v10 broker/client 必须一同回退，不能与 v9 混用。

## 验证

- Mock named-pipe supervisor 测试覆盖 stdin 背压重试、stdout/stderr 字节转发、session 运行中 gate-status 响应、stdin EOF 后 Stop 仍可接收，以及主动停止不伪造 exit code。
- 平台 locked tests、strict Clippy、格式和 ADR 索引检查在提交前运行。
- 未运行真实 HCS、quota 超额写入或 Windows 容器验收；这是用户明确要求暂不执行的发布验收。应用侧 MCP launcher 接线仍是后续工作。
