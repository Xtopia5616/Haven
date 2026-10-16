# ADR 1278：为 HCS guest stdin 实现有界写入

## 状态

已实现平台内受限 stdin writer 与匿名管道回归测试；尚未接入 broker MCP session。MCP stdio 在 `WorkspaceWrite` 下继续 fail closed。

## 背景

ADR 1273 已将 HCS guest stdin handle 纳入 `HcsGuestProcessOwner` 的校验和唯一所有权。HCS 返回的是同步 pipe handle；如果监督循环直接调用 `WriteFile`，guest 不读取 stdin 时写入可能无限阻塞，从而停止 stdout/stderr drain、进程状态轮询和撤权响应。现有 broker pipe 是单实例、单连接、串行处理的 Shell 控制通道，也不能直接承载长期 MCP session。

## 决定

- 增加私有 `HcsGuestProcessInputWriter`，每个 adapter 独占一个 guest stdin pipe，并把同步写入放在一个专用线程中。线程启动时发布自身 thread ID；父线程打开该线程的 `THREAD_TERMINATE` 访问句柄，以便用 `CancelSynchronousIo` 中断其同步写操作。
- adapter 只接受最多 16 KiB 的单个 chunk，并通过容量为 3 的同步队列非阻塞入队。加上 writer 当前持有的单个写入 chunk，Haven 保留的 stdin payload 最多 64 KiB；上游遇到 `WouldBlock` 时必须停止读取，等监督循环后续 poll 释放容量再继续。每个 chunk 在跨线程前复制，避免借用上游缓冲区。
- `close_input` 停止接收新 chunk；writer 按序排空已接受数据，随后关闭其 `File`，向 guest 发送 EOF。监督循环通过非阻塞 `poll` 确认 writer 完成，不在主线程等待管道写入。
- `abort` 先关闭队列并请求 HCS 终止 guest，再重复取消该 writer 线程的同步 I/O，最多等待 2 秒。成功退出后 join 线程；超时则返回未确认错误、分离线程并让线程继续拥有自己的 `File`，不从另一个线程关闭正在使用的 pipe handle。上层必须把未确认停止作为 attempt 不安全/待隔离处理，不能报告执行完成。
- HCS handle 所有权从 `OwnedGuestHandle` 一次性转给 `File`。当前 `NativeHcsApi::close_guest_handle` 对有效 guest handle 使用 Win32 `CloseHandle`；`File` 的 Drop 因此仍关闭同一 OS handle，避免双重关闭。adapter 持有 `&mut HcsGuestProcessOwner`，确保可取消期间 process owner 不会提前释放。
- 本切片只实现底层写入、缓冲、EOF 与取消 owner。不得让该 adapter 复用现有 one-shot broker 控制连接；后续需为 MCP generation 建立独立、经认证且可撤销的 session channel，并把其 framing、双向转发与每次 MCP tool call 授权接入后，才可开放受管 stdio profile。

## 替代方案

- 在 HCS 监督线程同步写 stdin：拒绝，满管道会阻止输出 drain 与取消/退出观察。
- 无界队列或一次性缓冲完整 MCP request：拒绝，输入速率或消息体可以无限占用 broker 内存。
- 仅丢弃 writer/thread handle：拒绝，guest 仍可能活着并继续占用 stdin pipe；无法确认停止。
- 把 MCP session 放到现有 Shell broker pipe：拒绝，该 listener 当前是单连接串行 owner，长会话会阻塞 Shell 与 status 控制请求。

## 影响与回滚

新增仅供 Windows HCS 使用的内部 writer，不改 broker wire protocol、MCP config、授权策略、持久化 schema 或 UI。当前没有生产调用方，私有 adapter 上的 dead-code lint allowance 只用于等待独立 broker session 通道接入；不得据此开放本地 stdio fallback。回滚时可删除 writer 模块与 stdin handle 到 `File` 的转移方法，并保留 ADR 1273 的所有权校验。无需重置数据库或配置。

## 验证

- Windows 匿名管道测试覆盖最大 chunk、总缓冲背压、排空后 EOF、拒绝关闭后的输入和阻塞同步写取消。
- 通过 `cargo fmt --all -- --check`、`cargo test --locked -p haven-platform`（201 passed、1 ignored）与 `cargo clippy --locked -p haven-platform -- -D warnings`。真实 HCS guest、broker session、权限撤销与 quota 验收未运行；这是用户明确选择的实现阶段，生产开放 gate 仍保持未关闭。
