# ADR 1236：将 broker 监听器生命周期接入 SCM 会话变更

## 状态

已采纳并实现。SCM stop/shutdown、会话变更与 named-pipe 取消共用一个信号；broker 现能认证连接并 fail closed 地拒绝尚未证明配额的执行请求。此决策不代表 HCS guest quota 已验收。

## 背景

ADR 1233 建立了受限、显式安装的 SCM 服务，ADR 1234 从唯一活动 WTS 会话派生 pipe peer policy，ADR 1235 为 named-pipe overlapped I/O 增加可取消事件和 deadline。此前这些机制尚未连接：服务只等待独立 stop event，没有绑定认证 listener，也不会在锁屏、切换用户或登录状态变化时废弃旧 logon SID ACL。

## 决定

- SCM `SERVICE_CONTROL_STOP` 和 `SERVICE_CONTROL_SHUTDOWN` 进入 `SERVICE_STOP_PENDING` 并触发 pipe cancellation；`SERVICE_CONTROL_SESSIONCHANGE` 触发同一取消事件。Stop 信号优先于 session-change，避免退出过程中重新建 listener。
- Listener 在服务线程中有界串行运行。会话变更取消当前 accept/read/write，关闭连接并销毁旧 listener，再从 WTS 获取新的活动 session、user SID 与 logon SID，并重建仅允许该 logon SID 和 broker service SID 的 pipe DACL。
- 零个活动交互会话或多个活动交互会话时，不创建 listener，服务等待下一次会话变化；WTS API、服务 SID 或 token 验证错误则使服务停止并报告失败。多活动会话仍不获准访问。
- Broker service SID 从固定的 `NT SERVICE\HavenContainerBroker` 账户解析，并须出现在当前服务 token 中。客户端不能为服务端提供 SID 或 session 身份。
- 已认证连接每次最多读取一个受限 request frame，读写各自采用 5 秒 deadline。HCS provider 与 runtime quota attestation 完成前，合法 `Execute` 请求返回带原 request ID 的 `quota_unattested`；其它非 Execute 消息返回 `invalid_request`。这里的 SCM `Running` 只表示服务循环存活，不代表执行后端 ready。
- 当前仅有一个串行 pipe 实例，仍只支持唯一活动交互会话。会话变更对所有活动连接执行保守撤销；不解析 event payload 来缩小撤销范围。

## 验证

- `cargo fmt --package haven-platform -- --check` 通过。
- `cargo check --locked -p haven-platform` 通过。
- `cargo clippy --locked -p haven-platform -- -D warnings` 通过。
- `cargo test --locked -p haven-platform`：75 项通过，1 项忽略（需要 HCS storage formatting privilege）。
- 单元测试未覆盖真实 Windows SCM/WTS session-change 通知；未进行 HCS 镜像启动、配额证明或 guest 超额写入验收。

## 后续工作

- 实现 broker HCS provider、workspace 输入与受限输出流，再接 Tools 侧 typed execution port 和 review owner。
- 在具备 Windows Containers/HCS、受支持镜像和 broker privilege 的环境中验收 scratch 消费协议、运行期 quota enforcement 与超额写入。该验收仍是 backend 对生产执行变为 ready 的硬门槛。

## 回滚

不得让服务重新接受没有 logon-scoped peer authentication 的连接，也不得在没有 quota runtime attestation 时执行 `Execute`。若 session-change 生命周期发生故障，保持 backend unavailable 并停止服务。
