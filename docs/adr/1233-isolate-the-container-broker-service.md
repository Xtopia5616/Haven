# ADR 1233：将容器 broker 安装为独立受限 Windows 服务

## 状态

已采纳并开始实施。独立 broker CLI 已支持显式安装、启动、停止、状态查询和卸载；SCM service dispatcher 已实现。当前 service 仅报告 SCM 生命周期状态，尚未接 named-pipe listener、HCS provider 或工具执行。

## 背景

HCS scratch 格式化需要普通 Haven 进程不持有的 Windows privilege；将高权限操作留在 Tauri 主进程会扩大所有 UI、插件和工具代码的权限范围。broker 因此必须是独立可安装进程，且服务身份、安装路径、权限和退出路径由平台层固定。真实 quota acceptance 仍由 ADR 1231 控制，不因为服务能够启动而开放执行。

## 决定

- 新增独立 `haven-container-broker` 可执行文件，提供 `install`、`start`、`stop`、`status`、`uninstall` 与仅供 SCM 启动的 `--service` 入口。服务名称唯一为 `HavenContainerBroker`；安装使用 demand-start、own-process 和 LocalSystem，不自动启动，也不接受任意 service name、account、binary path 或其他配置。
- 安装只接受 `%ProgramFiles%\\Haven\\haven-container-broker.exe`：Program Files 根由 Known Folder API 取得；目标目录和文件必须是普通非 reparse 路径；拒绝从用户可写目录注册 SYSTEM 服务。操作需要管理员权限，由调用者显式提升启动；broker 不自行弹 UAC。
- SCM 安装后配置 `SERVICE_SID_TYPE_RESTRICTED`，并通过 `SERVICE_REQUIRED_PRIVILEGES_INFO` 将服务 token privilege 白名单限制为 `SeChangeNotifyPrivilege`、`SeImpersonatePrivilege`、`SeManageVolumePrivilege`、`SeTcbPrivilege`。该集合服务 WTS 会话身份、pipe impersonation 与 scratch formatting；任何 HCS 调用失败都保持 backend unavailable，不回退到 unrestricted service SID、完整 LocalSystem token 或其他路径。微软说明 restricted service SID 会参与额外 restricted-SID access check，required-privileges 配置会移除未声明 privilege，并建议只保留最小集合。
- 安装遇到同名服务时失败关闭，不接管或改写既有 service。停止后最多等待 15 秒；超时不删除仍运行中的服务。卸载只删除 SCM registration，服务文件由应用安装包负责移除。
- `status` 只表示 SCM 生命周期状态，不代表 broker listener、HCS quota 或工具后端健康。当前 dispatcher 只报告启动/停止状态并等待 stop/shutdown；在后续 listener 与 provider 接线完成前，安装该服务不会启用产品执行。
- LocalSystem 的 HCS / WTS 权限组合尚无本机验收证据。restricted SID 和 privilege allowlist 是实现上的固定安全边界；若目标 HCS build 不接受该 token，保持 unavailable，不临时放宽。quota runtime attestation 与超额写入仍是生产执行的硬 gate。

## 验证

- 单元测试验证安装路径只允许固定 Program Files 子目录中的固定 broker 文件名，并拒绝其他位置。
- Windows service 安装、SCM 状态转换、restricted-token HCS 调用和超额写入均未执行；用户明确要求本阶段先实现功能、不安排实机验收。
- Microsoft 契约：[SERVICE_SID_INFO](https://learn.microsoft.com/en-us/windows/desktop/api/winsvc/ns-winsvc-service_sid_info)、[SERVICE_REQUIRED_PRIVILEGES_INFO](https://learn.microsoft.com/en-us/windows/win32/api/winsvc/ns-winsvc-service_required_privileges_infow)、[Service Security and Access Rights](https://learn.microsoft.com/en-us/windows/win32/services/service-security-and-access-rights)、[HcsFormatWritableLayerVhd](https://learn.microsoft.com/en-us/virtualization/api/hcs/reference/hcsformatwritablelayervhd)。

## 后续工作

- 将 WTS 活跃 session 的用户 SID、唯一 logon SID 和 session id 作为 broker peer policy 的唯一来源；处理登录/注销和无交互 session，不信任 renderer 或 pipe payload 提交的身份声明。
- 将 named-pipe accept/read/write 接入 service stop/session-change 控制；所有 accept、frame 与输出写入必须可取消、有 deadline 和累计容量上限。
- 把 broker binary 纳入正式 Windows 安装包，并在 service dispatcher 中接入上述 listener。尚未完成这些步骤时，SCM `Running` 不得投影为 backend ready。

## 回滚

通过 `haven-container-broker uninstall` 停止并移除服务注册即可回到当前 `WorkspaceWrite` fail-closed 行为。删除可执行文件不会改变用户数据；不得删除服务未停止时的 binary 或放宽其 image path 校验。
