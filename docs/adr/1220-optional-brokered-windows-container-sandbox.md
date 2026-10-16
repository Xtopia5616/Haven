# ADR 1220：采用可选 brokered Windows Container sandbox 后端

## 状态

已采纳架构方向，尚未接通生产工具。HCS backend 必须由用户/管理员显式安装和启用；当前仍需 HCS 超额写入、服务 IPC、workspace staging 与完整撤销/回收的 Windows 验收。任何条件不满足时，执行策略继续 fail closed。

## 背景

ADR 1209 要求 opaque 子进程拥有由 OS 在执行期强制的存储硬上限。现有 `AppContainerWorkspace`、Job Object、输出 manifest 和 pending-review budget 不限制普通临时目录在执行期增长。用户已选择接受可选 Windows Containers/Hyper-V 后端，并接受它要求容器宿主能力和特权服务。

HCS 是面向应用/管理服务的低层 VM 与容器管理 API；Microsoft 将容器 scratch 存储上限映射为 HCS `StorageSandboxSize`。HCS 本身不会替应用设置完整 host 资源生命周期，因此服务必须负责固定镜像、workspace staging、配额、隔离、超时、回收和崩溃清理。[HCS API](https://learn.microsoft.com/en-us/virtualization/api/hcs/overview)、[HCS API reference](https://learn.microsoft.com/en-us/virtualization/api/hcs/reference/apioverview)、[container resource controls](https://learn.microsoft.com/en-us/virtualization/windowscontainers/manage-containers/resource-controls)

Windows 容器还有独立的宿主和镜像许可范围。Windows 10/11 客户端容器宿主要求 Pro/Enterprise 与 Windows Containers 能力；Hyper-V isolation 还要求 Hyper-V。Microsoft 当前列出的 Client Host Software 镜像用途限于开发/测试；生产部署需要逐 host license 确认，不得从“Windows 能启动容器”推导出 Haven 可在该环境的生产后端使用。[Windows container requirements](https://learn.microsoft.com/en-us/virtualization/windowscontainers/deploy-containers/system-requirements)、[container base-image license](https://learn.microsoft.com/en-us/virtualization/windowscontainers/images-eula)、[containers FAQ](https://learn.microsoft.com/en-us/virtualization/windowscontainers/about/faq)

## 决定

### 后端与部署

- 新后端命名为 `WindowsContainerSandbox`，由 `haven-platform` 提供窄执行端口；HCS 与 Windows FFI 留在平台实现内。Tools 继续唯一负责工具授权、实时 policy permit、workspace capability、确认和输出晋升。
- 后端是显式可选部署组件：安装包不默认打开 Windows Containers/Hyper-V、不静默开启系统功能、不随产品下载或分发 Windows base image。用户/管理员单独安装 Haven Container Broker 与经许可的本机镜像/容器运行依赖。
- 支持矩阵以宿主探测和许可为双重条件：Windows 10/11 Pro/Enterprise 仅允许符合当前 Microsoft 条款的开发/测试配置；生产用途只有在 Windows Server 或适用 IoT/商业许可逐项核实后才能启用。未知 edition、许可状态、Windows build、HCS schema、Hyper-V isolation 或镜像匹配状态均视为不支持。
- broker 缺失、未启动、版本不匹配、能力探测失败、容器创建失败或配额字段不受目标系统支持时，绝不回退到普通 AppContainer temp、宿主 Shell 或 FullAccess；`WorkspaceWrite` 继续拒绝该 opaque execution。
- HCS quota backend 的接线不是授权范围放宽。Shell/Skill/MCP 还必须分别满足既有网络边界、confirmation 和 exposure policy；特别是 ADR 1181/1193 的 WorkspaceWrite OS 网络隔离验收未完成前，不解禁这些 opaque capability。

### Broker 安全边界

- broker 只调用本地 HCS API，不接受通用容器管理协议。IPC 使用本地命名管道，ACL 绑定安装时登记的 Haven 交互用户 SID；每次连接检查客户端 token、session、进程身份和签名安装路径，并执行带 nonce 的协议握手。未登记用户、远程客户端、服务降权/身份不明客户端一律拒绝。
- Broker API 是版本化 typed request，不收 HCS JSON、原始容器配置、镜像名、宿主 executable path、任意 host mount、任意 environment passthrough 或任意 pipe path。镜像、隔离级别、网络初始状态、资源预算和容器启动参数由已安装的受信任服务配置固定。
- 每次工具 attempt 创建独立、短生命周期的 Hyper-V-isolated compute system。服务只接受已准入的 executable/argv、有限环境变量、有限 stdin 和受限 workspace 文件流；command line 内容仍可表达用户授权的 Shell 请求，但服务永远不在 host 上执行它。
- 服务不接收或解释 Agent execution proof，也不提供确认逻辑。Agent/Tools 在进入 broker 之前完成 session/tool/capability/input/target 授权与实时策略检查；broker 仅以本机 IPC 身份和不可变执行配置做纵深防御。确认 UI、Trust 与 deny 状态不能由 renderer 或 broker 改写。
- broker 不读取任意用户路径。输入 workspace 由 Tools 从 canonical `ExecutionWorkspace` 经有界 capability snapshot 后，以文件流交给 broker；broker 验证规范相对路径、文件数/单文件大小/总字节数并在私有 service root 重建。只有该只读 staging tree 可映射入容器；任何可写映射目录都会绕过 scratch quota，禁止使用。
- 容器唯一的执行期可写面是 HCS 管理且配置了 `StorageSandboxSize` 硬上限的每 attempt scratch。容器退出后，service 在确认 compute system 和进程归零后生成有界输出 manifest/内容流；Tools 继续用现有 hash、review owner、目标 capability 与逐文件确认完成晋升。完整 workspace 或容器层不得自动复制回项目根。
- 不从 host 注入凭据、用户 profile、Credential Manager、Windows session handle、任意网络设备或 GPU；容器默认无网络。未来按现有 NetworkPolicy 增加受控网络，须单独 ADR 和 Windows 负向测试。
- broker 以执行 HCS 所需的最小特权身份运行；若目标系统实际要求 LocalSystem，则服务 IPC、调用方认证、固定操作面、更新/签名、服务 ACL 和服务崩溃恢复必须先完成 threat review。绝不能把 broker API 扩成通用 SYSTEM shell 或任意 HCS 管理接口。

### 配额、并发与生命周期

- 每次执行的 scratch quota 必须在创建 compute system 前设置，并在创建成功后经可查询状态或负向写入 probe 证明生效；字段缺失、单位/范围未知或验证不一致时不执行。
- active scratch reservation、workspace staging、pending review 与晋升中的输出必须归一个容量 owner 统一核算；每 attempt 上限、全局并发数、host scratch 总预算和失败恢复条件由实现 ADR 在 Windows 测量后固定，禁止仅依赖磁盘剩余空间采样。
- 每个 lease 具有 bounded wall-clock timeout、输出/事件/stdout/stderr 上限和显式 cancel；cancel、授权撤销、策略/边界更新、service shutdown 均停止 compute system，并等待子进程和后代归零后才释放 reservation 或删除 scratch。
- broker 使用稳定的 execution ID 记录生命周期，启动时只回收有 Haven owner marker、host root capability 与不活跃租约证据的残留；未知/部分创建对象隔离并报告，不按猜测路径删除。

## 替代方案

- 对所有 Windows 客户端默认启用 Windows Containers：拒绝；宿主能力、Hyper-V 和镜像许可不是所有 Haven 安装均具备。
- 让用户自行安装 Docker Desktop 或每用户 containerd 后即自动放开 `WorkspaceWrite`：拒绝；安装 runtime 不证明镜像许可、quota 实际生效、Broker 授权和 host/guest 网络隔离。
- 由高权限服务接受 HCS JSON 或宿主命令字符串并按调用方参数直接执行：拒绝；形成通用特权 confused deputy。
- 把项目根以可写目录映射进容器：拒绝；容器 scratch quota 无法限制该 host path 的写入。
- HCS 不可用时回退 AppContainer temp 或宿主进程：拒绝；普通 TempDir 无运行期容量上限，宿主进程又改变了 OS 权限边界。

## 影响与迁移

本 ADR 只确定可选部署边界，不新增 IPC/config/schema，也不改现有工具权限和拒绝行为，无需数据库或配置重置。它 supersedes ADR 1209/1215 对 HCS 作为尚未选择候选的当前状态；原生 API、NTFS 与 VHD 评估见 ADR 1219。实现按独立切片推进：typed sandbox port 与 mock、HCS scratch/quota prototype、broker IPC/installer、workspace staged execution、输出回传/review、撤销与崩溃回收、网络边界与各工具接线。任何切片未通过时不影响现有 fail-closed 路径。

## 验证

- 本 ADR 为架构决策；当前没有 Rust、IPC、配置、持久化或 UI 改动，不运行产品代码测试。
- Windows Container quota spike 必须以实际产品镜像和 Hyper-V isolation 在目标 Windows editions 上超额写入；证明 guest 获得文件系统容量错误、host 其它卷仍可写、scratch 上限不可由 guest 修改、进程树终止后磁盘资源被释放。
- Broker Windows 集成验收覆盖跨 SID/未签名 client/远程 pipe 拒绝、IPC 字段注入拒绝、path traversal/reparse 拒绝、任意镜像/mount/HCS JSON/host executable 不可请求、取消/撤销/服务崩溃清理和并发 quota reservation。
- Workspace 验收覆盖只读 staging、容器内不能读取 staging 之外 host 数据、无网络默认拒绝、受限输出 manifest、review quota 和未确认文件不晋升。
- Microsoft [HCS overview](https://learn.microsoft.com/en-us/virtualization/api/hcs/overview) 将 HCS 定位为控制 VM/container 的 API；[HCS API reference](https://learn.microsoft.com/en-us/virtualization/api/hcs/reference/apioverview) 列出 compute-system 与容器存储 API；[resource controls](https://learn.microsoft.com/en-us/virtualization/windowscontainers/manage-containers/resource-controls) 将 `StorageSandboxSize` 映射为 HCS 的容器 volume 控制。
- Microsoft [system requirements](https://learn.microsoft.com/en-us/virtualization/windowscontainers/deploy-containers/system-requirements) 与 [image license](https://learn.microsoft.com/en-us/virtualization/windowscontainers/images-eula) 是支持矩阵与分发门槛的权威输入。产品发布时必须重核版本和 license。

## 回滚

任何实现切片可通过关闭 `WindowsContainerSandbox` provider 并移除 broker 入口回滚；调用方回到当前策略拒绝行为。不得在回滚时把 WorkspaceWrite 调用静默切回 AppContainer 普通临时目录或 host process。
