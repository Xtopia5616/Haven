# ADR 1235：为 broker named-pipe I/O 设置取消与 deadline

## 状态

已采纳并实现平台传输原语。overlapped pipe 尚未接入 SCM service listener；在服务生命周期接线前，broker 仍不接受产品执行请求。

## 背景

broker 协议已限制 frame 大小，但同步 `ConnectNamedPipe`、`ReadFile` 或 `WriteFile` 仍可能在无客户端或客户端停止读取时长期阻塞。SCM stop 与 session-change 必须能够中断这些等待，协议 deadline 也不能被阻塞写绕过。

## 决定

- 服务端 pipe 和客户端 pipe handle 均以 `FILE_FLAG_OVERLAPPED` 打开；每个 connect/read/write 操作使用独立 manual-reset event 和 `OVERLAPPED`，等待时同时监视 I/O 完成与 cancellation event。
- 取消或超时时调用 `CancelIoEx`，并等待操作完成事件后才释放 `OVERLAPPED` 与缓冲区；连接握手在 5 秒 deadline 内完成。读写 frame deadline 在一帧内共享，调用超时不得超过 60 秒。取消以 `Interrupted`、deadline 以 `TimedOut` 返回。
- wire frame 仍由 ADR 1232 的协议上限约束：control payload 最大 320 KiB，binary chunk 最大 64 KiB；一次读取按长度字段验证后才分配 body buffer。输出流应继续使用小型 binary frame，不能把完整 ToolRun 输出缓存在 pipe 层。
- cancellation event 是一次性取消信号。SCM handler 后续将用同一 event 中断 listener loop；session-change 后丢弃旧 listener/policy，再从 WTS 创建新快照。当前该连接尚未实现。
- 平台测试覆盖无数据时读 deadline、客户端停止读取时写 deadline、accept 被取消、握手认证与 wrong-user 拒绝。真实 SCM shutdown/session-change 行为尚未验收。

## 验证

- Microsoft 的 named-pipe overlapped 示例要求 `ConnectNamedPipe`、`ReadFile`、`WriteFile` 使用有效 `OVERLAPPED`，并在 pending 完成后读取结果；实现为每次操作单独建 event。[Named Pipe Server Using Overlapped I/O](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-server-using-overlapped-i-o)
- Microsoft 说明 `CancelIoEx` 发起取消后仍需等待操作完成，且取消请求本身不保证底层操作一定取消；实现保持操作结构与缓冲区有效直到 event 完成。[Canceling Pending I/O Operations](https://learn.microsoft.com/en-us/windows/desktop/FileIO/canceling-pending-i-o-operations)

## 后续工作

- 用该 cancellation event 取代 service 内独立 stop event；SCM stop/shutdown 与 session-change 都要使等待的 accept、read、write 返回，再完成 service 状态转换。
- 处理已连接 peer 的 session 变更：先断开旧 pipe，再重新读取 WTS 身份与创建 DACL，不能继续用陈旧 policy 接受请求。
- 接入 HCS provider 时保留相同 frame/deadline 上限，并用有界队列与 producer backpressure 控制输出。

## 回滚

不得回退到可无限阻塞的同步 listener。若 overlapped I/O 初始化或取消失败，关闭当前连接并让 backend unavailable。
