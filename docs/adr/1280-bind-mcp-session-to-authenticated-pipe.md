# ADR 1280：将 MCP generation 绑定到独立认证管道

## 状态

已实现独立 named-pipe 创建/连接与 attach identity 校验，并通过 Windows named-pipe 单测。服务尚未处理 `McpSessionOpen`、mint session ID、启动 HCS MCP process 或 relay stdio；`WorkspaceWrite` 下 managed stdio MCP 仍 fail closed。

## 背景

ADR 1279 定义了 broker MCP generation 的独立 pipe、Hello 和 `McpSessionAttach`，但 `named_pipe` 只有 one-shot 控制管道。若直接复用控制管道，长会话会阻塞 quota status 与 Shell request；若只从客户端传入 pipe 名或 session ID，则破坏 broker 对 session 身份的所有权。

## 决定

- 在现有 named-pipe transport 中增加单 generation listener/client 构造器；pipe 名由固定前缀和 16 字节 broker-issued session ID 的小写 hex 派生。全零 session ID 与 request ID 均拒绝。
- session listener 继续使用既有 `BrokerPeerPolicy` 安全描述符、`FILE_FLAG_FIRST_PIPE_INSTANCE`、`PIPE_REJECT_REMOTE_CLIENTS`、交互 logon SID ACL、overlapped deadline I/O 和 Hello version negotiation；listener 创建前验证当前 broker service SID。
- 客户端必须经 `BrokerClientPolicy` 验证当前进程身份，连接后再验证 pipe server 是安装的 SCM service。Hello 成功后立即发送 `McpSessionAttach`。
- listener 的 `accept_mcp_session` 在返回连接前消费首条控制帧，并要求 request/session pair 与创建 listener 时的 broker session ID 精确匹配；错误身份拒绝并断开，不暴露 session 数据帧给上层 handler。
- 仅增加 pipe transport primitive；随机 session ID 的生成、`McpSessionOpen` 授权、session listener 生命周期、HCS process owner、双向 stdio relay、撤权与 stop confirmation 必须由后续受监督 service owner 实现。当前不为 `NativeHcsApi`、workspace capability 或 attestation 增加跨线程 `Send`。
- MCP session listener 与 one-shot control listener 独立，因此长期会话不占用控制 pipe。每个 session pipe 单实例，attach 阶段使用 5 秒握手 deadline；帧仍受 ADR 1279 的 16 KiB 限制。

## 替代方案

- 复用控制 pipe：拒绝，会占用控制面并阻塞 status/Shell 请求。
- 让客户端选择 pipe 名或 session ID：拒绝，broker 必须拥有 session identity，客户端只可回显其收到的 open pair。
- 在本切片启动 detached HCS thread：拒绝，HCS API、attestation 与 workspace owner 的线程安全和停止生命周期尚未证明。

## 影响与回滚

新增仅供 brokered MCP generation 使用的 Windows named-pipe API，不改持久化数据、配置或 UI，无需重置。当前无 service 调用点，未改变 `WorkspaceWrite` 授权结果。回滚时删除 session listener/client API 与相关 tests/ADR 更新；`McpSessionOpen` 继续按现有行为拒绝。

## 验证

- Windows named-pipe tests 覆盖派生管道名、零 session ID 拒绝、完整 Hello/attach/ready 握手以及 request ID 不匹配拒绝。
- 本切片完成后运行平台 crate locked tests、strict Clippy、格式检查和差异检查。
- 未启动真实 broker service 或 HCS MCP guest；Quota/HCS acceptance 仍按用户要求暂不执行。
