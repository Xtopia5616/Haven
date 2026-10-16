# ADR 1234：从 WTS 活跃会话派生 broker peer policy

## 状态

已采纳并实现身份快照原语。broker listener 还没有接入该 policy，也没有注册 session-change 通知；在该接线完成前本 ADR 不代表 broker 可以执行工具。

## 背景

Named-pipe 认证要求服务端先知道预期用户 SID、logon SID 与 session id，随后再通过客户端 impersonation token 校验连接。若这些值由 renderer、命令参数或 pipe payload 提供，恶意调用者可以把身份策略改成自己的身份。LocalSystem 服务可以通过 WTS 枚举活跃 session，并使用对应 session 的 user token 得到内核身份字段。

## 决定

- 服务端的 `BrokerPeerPolicy::from_active_session` 从本机 WTS session list 与 `WTSQueryUserToken` 派生用户 SID、logon SID 和 session id；服务 SID 由 broker 固定 SCM 配置传入。客户端的 `BrokerClientPolicy::for_current_process` 则从自身进程 token 派生身份字段，不要求普通用户进程调用特权 WTS API。两个入口都不接受 renderer 或 pipe payload 提供的身份字段。
- 只选择 `WTSActive` 且 session id 非零的交互 session，并核对取得的用户 token 的 `TokenSessionId` 仍与枚举 id 相同。WTS 缓冲区和 primary token 均由 RAII 释放。
- 当前单 pipe instance 仅支持恰好一个活跃交互 session。没有活跃 session 时返回 unavailable；多个活跃 session 时拒绝选择，不按枚举顺序挑选用户。以后若要支持多用户/RDP 并发，必须用每会话 pipe instance 与各自 DACL 扩展，不得放宽为任意 logon SID。
- policy 是一次性快照。后续 listener 接线必须处理 session logon、logoff、disconnect 与 reconnect，并在身份变化时撤销旧 pipe；当前实现不缓存或持久化这些 SID。
- 代码级测试覆盖 session 0 排除、唯一活跃 session 选择、无会话与多会话拒绝，以及客户端 policy 与当前进程 token 一致。WTS 服务调用和 session-change 行为未在真实安装服务上验收。

## 验证

- Microsoft 要求 `WTSQueryUserToken` 调用方运行于 LocalSystem 且具备 `SeTcbPrivilege`，并要求关闭返回的 token handle；服务 allowlist 已保留该 privilege，代码以 RAII 关闭 token。[WTSQueryUserToken](https://learn.microsoft.com/en-us/windows/win32/api/wtsapi32/nf-wtsapi32-wtsqueryusertoken)
- `WTSEnumerateSessions` 返回 session 列表，调用方需释放 WTS 分配的 buffer；实现限制处理的 session 数量并通过 `WTSFreeMemory` 回收。[WTSEnumerateSessionsW](https://learn.microsoft.com/en-us/windows/win32/api/wtsapi32/nf-wtsapi32-wtsenumeratesessionsw)

## 后续工作

- 在服务注册 session-change handler，把通知与 stop/shutdown 连接到同一个取消信号；session policy 变化时关闭旧 pipe 并重新读取 WTS 身份。
- 将 accept/read/write 改为可取消、有 deadline、帧和累计输出均有界的 listener loop，并限制阻塞写入的背压。
- 等 listener、session-change 和 broker provider 集成后，继续以 runtime quota attestation 控制 backend readiness。

## 回滚

移除 WTS policy 构造入口会使 broker 无法创建生产 peer policy；调用方不得恢复从 renderer 或 pipe payload 接收用户 SID、logon SID 或 session id。
