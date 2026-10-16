# ADR 1284：允许 MCP session supervisor 有界轮询 broker control pipe

## 状态

已实现 transport primitives 并通过 Windows named-pipe test。实际 service event loop 尚未接入；ADR 1283 HCS owner 当前仍无 service 调用点。

## 背景

ADR 1280 为 MCP generation 提供独立 authenticated pipe，ADR 1283 要求 HCS 生命周期留在受监督的 service 调用线程。当前 control listener 的 accept 在无客户端时持续等待；如果 session pump 占用同一线程，它不能在每次 HCS poll 之间检查 gate-status/control 请求。再开 HCS worker thread 会错误地跨线程传递 `NativeHcsApi` 和 attestation。

## 决定

- 新增 `BrokerPipeListener::accept_with_timeout`，只限制等待 client connect 的时间；一旦连接到达，仍执行原有 bounded Hello、version negotiation、peer token impersonation 与 auth handshake。
- 超时仅断开当前未连接 pipe instance，不销毁 listener；其后仍可接受下一条经过身份认证的控制连接。timeout 必须为正且不超过现有 60 秒 pipe I/O 上限。
- 已认证 `BrokerPipeConnection` 可以基于其 `BrokerPeerPolicy` 派生一个独立 MCP session listener；policy 不接受客户端构造，仍重新验证 broker service SID，session ID 必须非零并用于固定管道名。
- 后续 service supervisor 在 session HCS/stdio polls 之间以短 timeout 轮询 control listener，只即时处理 status，并对同一单槽执行期间的其他执行/open 请求 fail closed。HCS、workspace、quota proof 和 attestation 继续留在该线程；此 ADR 本身尚未实现该 event loop 或策略撤权。
- 不增加 thread、额外 HCS owner 或无界队列；本切片不改变当前 control pipe 名、协议版本、持久化与 `WorkspaceWrite` 授权结果。

## 替代方案

- 将 HCS API 与 attestation 发往 detached worker，并由另一个 control thread 服务状态：拒绝，当前类型未证明 `Send`，会破坏既定 owner 生命周期。
- 在 accept timeout 后关闭整个 control listener：拒绝，会让下一条状态查询需要重启 listener，并产生竞态窗口。

## 影响与回滚

仅增加 Windows named-pipe 私有 API、测试和后续 supervisor 的接线能力，不改持久数据或公共 wire schema，无需重置。回滚时删除 timed accept 与派生 session-listener API；`WorkspaceWrite` 下 MCP 继续 fail closed。

## 验证

- named-pipe test 验证 accept timeout 与零 timeout 拒绝，并确认同一 listener 随后成功完成认证握手且收到 typed MCP open request。
- 本切片完成后运行平台 locked tests、strict Clippy、格式和 ADR 索引检查。
- 未启动真实 broker service/HCS MCP guest；Quota/HCS acceptance 按用户要求暂不执行。
