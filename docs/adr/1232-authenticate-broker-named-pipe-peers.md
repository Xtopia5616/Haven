# ADR 1232：认证 broker named-pipe 两端并按 logon SID 限制访问

## 状态

已采纳并开始实施。Windows 平台层已有 named-pipe listener/client 与 peer 校验；service installer/lifecycle 尚未接线，因此产品 backend 仍 unavailable。

## 背景

ADR 1231 允许在真实 HCS quota runtime 验收前实现 broker，但不放开生产执行。broker 是 HCS 存储操作的权限边界，不能依赖 named pipe 默认 ACL、可伪造的进程参数或客户端发送的身份字段。连接也必须限定在本机交互式登录，避免远程管道和其它用户/session 的访问。

## 决定

- 仅使用固定的本地 `\\.\pipe\Haven.ContainerBroker.v1` byte-mode pipe，启用 `PIPE_REJECT_REMOTE_CLIENTS`，并传入受保护的显式 DACL；不依赖 Windows 默认 named-pipe ACL。ACL 只授予已配置 broker service SID 与当前 logon SID。listener 使用 `FILE_FLAG_FIRST_PIPE_INSTANCE`，并在断开客户端后保留同一 server handle，避免释放管道名后被本地进程抢占。
- 客户端首帧只能是 protocol `Hello`。服务端先以 1 KiB 上限和绝对 5 秒读期限读取该控制帧，再 impersonate named-pipe client，从 thread token 取 `TokenUser`、唯一 logon SID、token session，并与管道连接的 PID/session 及已配置 peer policy 比对。身份读取使用 `OpenThreadToken(..., OpenAsSelf=TRUE)`；任何失败或不匹配都断开连接，不发送 `HelloAccepted`。所有返回路径都先恢复服务线程身份；若 `RevertToSelf` 失败则立即终止服务进程，避免它继续以客户端身份处理特权请求。
- `HelloAccepted` 后服务端才把连接交给 broker handler。数据帧依旧由协议层限制大小；连接只暴露带期限的 frame 读取方法，不暴露无期限的 `Read` stream，避免 handler 绕过 framed read limit。
- 客户端在发送 Hello 前验证 named-pipe server PID 与 SCM 中正在运行的 `HavenContainerBroker` 服务 PID 一致，并要求 server 位于 Session 0。服务端创建 listener 时检查当前进程 token 含 broker service SID 且位于 Session 0。这样客户端不需要打开或查询受限 service process token。
- peer 身份只证明 Windows 用户/logon/session 与安装服务进程边界；它不承载 Haven 的 permission proof、workspace capability 或 session trust。broker 仍只接受固定 typed 请求，不接收绝对路径、host mount、HCS JSON 或任意 executable/environment。相同 logon session 中的其它进程也属于该 OS 身份边界；防止模型代码绕过 Haven 仍依赖模型执行不能访问 host named pipe，以及 broker/HCS 的固定受限能力。
- peer transport 当前串行接收一个 pipe instance。service 生命周期后续必须负责会话登录/注销时的 policy 更新、可取消的 accept/shutdown 和有界写入/backpressure；未完成前不得将 listener 接入生产工具。

## 验证

- 普通 Windows 测试覆盖真实本机 pipe 的合法 Hello/impersonation、错误 user SID 拒绝、纯 peer policy 的 user/logon/session/PID 检查，以及对停发帧的绝对读超时。
- 这些测试不验证 SCM 安装身份、service SID 配置、HCS 启动或 guest quota。HCS 配额实机验收仍按 ADR 1226/1230 延后，并继续受 ADR 1231 的生产启用 gate 约束。
- Microsoft API 契约：[CreateNamedPipeW](https://learn.microsoft.com/en-us/windows/win32/api/namedpipeapi/nf-namedpipeapi-createnamedpipew)、[Named Pipe Security and Access Rights](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights)、[ImpersonateNamedPipeClient](https://learn.microsoft.com/en-us/windows/win32/api/namedpipeapi/nf-namedpipeapi-impersonatenamedpipeclient)、[RevertToSelf](https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-reverttoself)、[SERVICE_SID_INFO](https://learn.microsoft.com/en-us/windows/desktop/api/winsvc/ns-winsvc-service_sid_info)。

## 后续工作

- service lifecycle 需显式安装/卸载、配置 restricted service SID 与所需最小权限、为已登录用户 provision peer policy，并保证 query service status 的 ACL 可供 Haven 客户端验证。
- 为 accept 与写入增加停止/超时语义；增加完整消息状态机、workspace/output 累计配额与分块流控，确保一条恶意或停止读取的连接不能永久占用 broker。
- HCS provider、staging/cleanup 和 Tools 接线均保持 quota runtime health gate 关闭，直到目标 Windows 超额写入验收通过。

## 回滚

不创建 listener、不安装 broker service 即可回到当前 `WorkspaceWrite` fail-closed 行为。删除 peer transport 时，不得改用默认 pipe ACL、移除远程客户端拒绝或把 pipe 身份当作 Haven 工具授权。
