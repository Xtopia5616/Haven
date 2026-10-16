# ADR 1279：定义 brokered MCP generation session wire protocol

## 状态

protocol v9 的 typed 消息和有界 stdio frame 已实现并通过平台单测；独立 named-pipe listener、service handler、HCS launch 与双向 relay 尚未接通。`WorkspaceWrite` 下 managed stdio MCP 仍 fail closed。

## 背景

ADR 1269 要求每个 `haven-mcp` client generation 对应一个 HCS guest process generation，且由 `haven-mcp` 独占 MCP/JSON-RPC framing、请求关联和取消。ADR 1278 已提供有界 HCS stdin writer，但现有 broker 控制 pipe 是单实例的 one-shot Shell/status owner，不能承载长生命周期 stdio 字节流。两端需要一个独立 session pipe 和可验证的 attach/ready/close 语义。

## 决定

- Broker protocol 升至 v9。当前 client/server 只协商同一版本，旧 v8 二进制不能同新服务建立 session；不增加兼容回退。
- 控制 pipe 上的 `McpSessionOpen` 仅携带非零 `request_id` 与经 Common `is_valid_mcp_package_id` 校验的受管 `package_id`。它不接受 command、argv、environment、cwd、host path、workspace、secret、任意 runtime 或 HCS JSON。service 必须从启动时 pinned image catalog 重新解析 package，并检查启动 quota、Node runtime 与该 package 的 process-local proof。
- `McpSessionOpenAccepted` 返回 broker 生成的非零随机 `session_id`。客户端不能选择 pipe 名或 session ID；service 根据 ID 创建独立的 per-generation authenticated pipe。session ID 只属于进程内 generation，不是持久实体，不套用数据库 `new_id` 格式。
- 新 pipe 仍执行现有 Hello/version handshake 和 named-pipe peer/server 身份校验；随后首条 session control 必须是 `McpSessionAttach { request_id, session_id }`，且与控制 pipe 的 accepted pair 精确匹配。HCS process 创建并通过 start boundary 后，service 才发 `McpSessionReady`；客户端在 Ready 前继续持有 MCP process-start permit。
- session pipe 上 `McpStdinChunk`、`McpStdoutChunk` 和 `McpStderrChunk` 是分开的 binary frame kind，每帧严格限制为 16 KiB，对应 ADR 1278 writer chunk；不能借用一般 workspace/output frame 的 64 KiB 上限。上层达到缓冲上限时停止读另一端，保留字节顺序并传播背压，不能丢弃或截断 MCP stdout JSON-RPC 字节。
- `McpSessionInputClosed` 表示客户端不再写入；service 排空已接受的 stdin 后关闭 guest stdin，使 guest 收到 EOF。`McpSessionStop` 要求回收 generation。只有 HCS 确认整台 compute system 已停止后，service 才能发送 `McpSessionExited` 并释放 scratch；断连、策略撤销、deadline、写入错误或停止不确定均走现有 quarantine/fail-closed 语义。
- `stderr` 是独立诊断流，不能作为 MCP protocol data，也不能判定 tool result 成败。Haven MCP transport 仍消费 `stdout` 和 JSON-RPC；broker 只搬运字节。
- 独立 pipe 不得通过无界 detached HCS 执行线程绕过 `QuotaRuntimeAttestation`/workspace attempt owner。后续 service slice 必须在 broker service 的受监督生命周期中接入 session 并同时保持控制 pipe status 可响应；不得给 `NativeHcsApi`、workspace capability 或 attestation 加未经证明的跨线程 `Send`。
- 本 ADR 只实现 wire types、ID/package 校验和 frame size guard。尚未创建 listener、发送 session frames 或启动 HCS MCP process；所有缺失集成入口继续拒绝。

## 替代方案

- 复用 Shell 控制 pipe 传输长会话：拒绝，会占用 status/Shell control connection，并破坏 one-shot request 的关闭边界。
- 让客户端指定 session ID/pipe 名：拒绝，会引入 pipe squatting 与跨请求绑定问题；session identity 应由 broker mint。
- 复用通用 64 KiB binary chunk：拒绝，stdin 写入 owner 以 16 KiB chunk/64 KiB 总窗口为容量模型；更大的 wire chunk 会破坏背压证明。
- 在 broker 解析或重写 MCP JSON-RPC：拒绝，会复制 `haven-mcp` protocol owner 并改变 bytes-in/bytes-out 语义。
- 为让 listener 并行工作而把 HCS API、attestation 或 capability 强制标成 `Send`：拒绝，HCS session 仍须由受监督 owner 驱动，跨线程安全需要独立证据与生命周期设计。

## 影响与回滚

新增仅供 Windows Container broker 的 protocol v9 请求、session lifecycle controls 和 MCP-specific binary frames。没有数据库、配置、持久身份或 UI 变化，无需重置数据。回滚时将协议退回 v8 并删除未接入的 MCP message/frame variants；managed profile 仍在 host spawn 前拒绝，因此不产生运行时残留。

## 验证

- 平台协议单测覆盖 v9 control round-trip、非零 request/session identity、managed package ID 校验、拒绝额外 host command 字段和 16 KiB 各向 frame 上限。
- 通过 `cargo fmt --all -- --check`、`cargo test --locked -p haven-platform`（203 passed、1 ignored）及 `cargo clippy --locked -p haven-platform -- -D warnings`。
- 本切片未启动 named-pipe session、HCS guest MCP process 或真实 quota probe；服务接线与 Windows HCS 验收仍未完成。
