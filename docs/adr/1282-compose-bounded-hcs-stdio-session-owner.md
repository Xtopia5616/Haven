# ADR 1282：组合有界 HCS guest stdio session owner

## 状态

已实现为私有平台 owner，并通过匿名管道 tests。它尚未连接 broker MCP listener/service；stdio MCP 在 `WorkspaceWrite` 下继续 fail closed。

## 背景

ADR 1281 让有界 stdin writer 独占 HCS process owner，ADR 1273 已将 stdout/stderr handle 转为 capability-like owned handles。若 broker session 自己拼装这些对象，每个消费者都必须重复遵守“先转移两个输出句柄，再按值移交 process”及 abort/EOF 生命周期。

## 决定

- 增加私有 `HcsGuestProcessStdioOwner`，按值接收一个创建了 stdin/stdout/stderr pipes 的 HCS process。
- 构造时先把 stdout/stderr 转移给 `HcsGuestProcessOutputReader`，再由 `HcsGuestProcessInputWriter` 接管 process 和 stdin。任一构造步骤失败时，未转移资源按现有 owner Drop 规则清理。
- owner 仅暴露小接口：有界 stdin chunk、stdin EOF、writer completion、process exit poll、双流 bounded drain 和 abort；不传 raw HANDLE 或 HCS API。
- 既有每帧 16 KiB、总输入 64 KiB、输出每 stream 每 poll 64 KiB、同步写取消上限和 process stop 语义保持不变。owner 不做 JSON-RPC framing，也不丢弃/解析 MCP payload；调用方负责把 stdout bytes 通过独立 session pipe relay。
- 该类型只建立 HCS 侧会话资源单位；scratch/quota proof、MCP package 证明、授权、session identity、broker service multiplexing 和真实 HCS 验收仍归现有各自 owner。

## 替代方案

- 让每个 broker transport 消费者分别持有 reader/writer：拒绝，会复制 handle 转移和关闭顺序规则。
- 将 stdout/stderr 与 stdin 合并为一个同步 worker：拒绝，会让被阻塞的 stdout 写或 guest stdin 写阻止另一流和 process 退出监督。
- 在平台 owner 中解析 JSON-RPC：拒绝，`haven-mcp` 是唯一的 framing、请求关联与取消 owner。

## 影响与回滚

只增加私有 HCS stdio composition type 和测试，不改持久化、配置、IPC/UI 或生产授权，无需重置。回滚时移除该 wrapper，并在未来实际 launcher 中直接组合 ADR 1273/1278 owners。

## 验证

- 平台 tests 验证统一 owner 保留 stdout/stderr 原始字节、stdin 有序写入与 EOF、背压和 abort 时 process 终止。
- 本切片完成后运行平台 locked tests、strict Clippy、格式与 ADR 索引检查。
- 未启动真实 broker service/HCS MCP guest；Quota/HCS acceptance 仍按用户要求暂不执行。
