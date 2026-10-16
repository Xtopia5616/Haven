# ADR 1231：先实现 broker，再以运行期配额验收控制启用

## 状态

已采纳并开始实施。允许在真实 HCS quota runtime 验收前实现 broker、IPC、生命周期和工具接线；未经目标 Windows 实测证明的 backend 必须保持 unavailable，不能执行 `WorkspaceWrite` opaque capability。

## 背景

ADR 1226 将 guest 超额写入证明设为 broker IPC/installer 的前置条件。用户随后明确要求先实现功能、暂不安排真实验收。实现仍需遵守最小权限、失败关闭、版本化 IPC、workspace 不传绝对路径和逐次授权等安全契约；缺少宿主 runtime 证据不能被 mock 或序列化测试替代。

## 决定

- 将“开始构建 broker/IPC”与“允许生产 backend 执行”拆成两个 gate。代码、broker service 和 UI/Tools 接线可先落地；只有明确受支持的 Windows build、Hyper-V isolation、产品兼容镜像、scratch 配额、guest 超额写入拒绝和停止后清理都实测通过，运行时 health 才能报告 backend 可执行。
- 未取得 quota attestation、HCS API/镜像能力不匹配、协议版本不兼容、service 不可信或运行时状态不确定时，client/backend 统一 fail closed；不得回退到普通宿主进程、AppContainer TempDir、`StorageSandboxSize` 声明或可写 host mount。
- 每个实施切片仍须增加可在普通 Windows CI 上运行的契约/负向测试，并更新 ADR/路线图、按项目门禁提交。真实 HCS 环境验收延后，不得把测试跳过描述成通过。
- broker 只执行经过结构校验的受限 Shell 请求和边界内的 staging/review；不持有 Haven 工具授权、permission proof 或 session trust owner。每次执行的授权、撤销、目标 workspace generation/policy revision 仍由 App/Tools owner 管理。
- IPC 必须有固定版本与有界 frame；不接收 HCS JSON、任意 executable/environment、绝对 workspace path、host mount、网络端点或任意服务控制指令。客户端与 broker 均验证 request limits，pipe peer 认证在 transport owner 完成。
- 当前第一个实现切片是 versioned bounded frame/control contract；随后实现 Windows named-pipe peer authorization、service lifecycle、broker HCS provider、输入/输出流和 review 续接。工具可接线但保持 quota health gate 禁用。

## 替代方案

- 等真实 HCS 环境准备好以后再开始写任何 broker/IPC：按用户当前指令不采用；它会把可由静态契约、Windows 单元测试和 mock 验证的代码工作与环境专属验收绑定。
- 将 mock、HCS JSON 序列化或 VHDX 元数据检查当作运行期配额通过：拒绝；它们不能证明 guest 磁盘写入收到容量错误。
- 实现并默认启用 broker，验收留到发布后：拒绝；会让未验证 backend 越过 `WorkspaceWrite` 的 fail-closed 边界。

## 影响与迁移

本 ADR 调整 ADR 1226 的实现顺序，不改变其 quota 安全不变量。无数据库 schema 或用户配置迁移；新增 service 只有在显式安装后启动，应用默认仍不依赖它。broker protocol 与 service install lifecycle 完成后再更新配置/发布说明和卸载清理契约。

## 验证

- 每个协议/授权/service 切片执行项目规定的 crate 与跨端门禁，并覆盖 malformed/oversized frame、版本拒绝、未授权 peer、取消/超时、策略撤销、staging 清理与孤儿恢复。
- 真实 HCS runtime quota acceptance 明确延期，不作为本 ADR 的完成证据；最终启用 gate 仍需 ADR 1226/1230 规定的目标机验收记录。

## 回滚

关闭 broker service 与 client 注册/工具接线即可回到当前 AppContainer 与 `WorkspaceWrite` fail-closed 行为。协议/服务代码可以保留为不可启动的内部实现，或按后续 slice 回滚；不得通过删除 health gate 来规避失败。
