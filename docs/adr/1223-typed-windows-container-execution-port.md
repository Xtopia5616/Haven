# ADR 1223：定义 typed Windows Container 执行端口

## 状态

已实施平台内执行契约与 mock；尚无 HCS provider、broker 或生产工具接线。所有当前工具授权行为保持不变。

## 背景

ADR 1220 选择可选 brokered Windows Container/HCS 作为未来有硬磁盘配额证明的 opaque 执行后端。平台已有固定 HCS 配置构造器和后端中立的 `SandboxOutputReview`，但缺少连接 Tools 与平台 provider 的窄执行契约。接口必须让 Tools 持有授权、实时策略 permit、取消与输出审阅，同时避免把通用 HCS JSON、宿主路径或权限凭据变成 broker API。

## 决定

- `haven-platform::windows_container::execution` 定义 sealed `WindowsContainerExecutionPort`。Tools 可以消费平台选择的 provider，但不能注入任意执行实现。
- 每次请求只接受受限 shell 类型、非空且有字节上限的命令、workspace 相对 cwd、workspace generation/policy revision stamp，以及经校验的 scratch 配额（1–4 GiB）、超时（复用产品 3600 秒硬上限）、每流 stdout/stderr、输入快照和输出 manifest 限额。
- 请求不携带 execution proof、session identity、HCS JSON、镜像选择、宿主 executable、任意环境变量、host mount、网络端点或凭据。平台 provider 只能使用其受信任配置的镜像和固定 HCS 构造器。
- provider 接收已授权 workspace 的 `DirectoryCapability`，只能通过受限文件流为 broker staging；绝对 root 不得跨 broker IPC。guest cwd 必须是清洁相对路径。输入/输出树仍由现有 copy/diff 限额约束。
- 执行端口提供取消查询与有界实时输出回调。provider 在转发每个输出块前必须核算每流字节数；超出限额即失败。只有进程树归零、输出流已完整关闭且运行期 quota 已验证后，才允许返回结果和 `SandboxOutputReview`。
- 端口采用同步调用，供 Tools 在既有 blocking worker 中持有实时 policy permit 并等待 OS 执行生命周期；取消、超时、暂存错误或 quota 无法证明时均返回错误，不回退宿主进程或普通临时目录。
- 当前只有契约和 mock 测试；不创建 HCS compute system、不安装/连接 broker、不放开 Shell/Skill/MCP，也不改变 `WorkspaceWrite` 对 opaque capability 的拒绝。

## 替代方案

- 让 Tools 直接调用 HCS 或 broker JSON API：拒绝；会将宿主 OS 细节和通用高权限协议泄漏到工具执行层。
- 让端口携带 proof、session 或目标授权：拒绝；授权 owner 仍在 Tools，broker 不得签发或解释权限。
- 让任意调用方实现并注入 port：拒绝；执行后端、镜像与隔离配置由 `haven-platform` 受信任选择。
- 把整个 workspace 或 host root 通过可写挂载传给容器：拒绝；这会绕开 scratch quota 与逐文件晋升。

## 影响与迁移

新增平台 Rust API 与 mock 单元测试；不改数据库、持久配置、IPC、权限配置或用户可见行为，不需要数据/配置重置。真实 provider 后续可在同 crate 内实现该端口，并通过 Tools composition root 接入；跨 broker 的协议仍须定义独立版本化 DTO，不能直接序列化本端口对象。

## 验证

- `cargo fmt --all -- --check`
- `cargo test --locked -p haven-platform`
- `cargo clippy --locked -p haven-platform -- -D warnings`
- mock 覆盖请求字段、取消、路径拒绝、限额校验、输出截断与 review owner 返回。
- 本切片不构成 HCS quota runtime 证明。目标 Windows 版本上的 HCS 超额写入、workspace staging、broker 认证、撤销/回收和网络隔离仍由后续 ADR/Windows 集成验收处理。

## 回滚

删除 `windows_container::execution` 接口及其 mock 即可；当前无 provider 或工具 consumer，回滚不改变授权、安全或执行行为。
