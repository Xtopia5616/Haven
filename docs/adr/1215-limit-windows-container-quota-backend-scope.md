# ADR 1215：收窄 Windows Container 配额后端的适用范围

## 状态

已采纳。HCS `StorageSandboxSize` 保留为特定宿主部署的候选，不作为 Haven 常规 Windows 客户端生产执行的默认配额后端。

## 背景

[ADR 1209](1209-require-os-enforced-sandbox-storage-quota.md) 将 HCS `StorageSandboxSize` 留作可选 OS 存储配额候选。后续核对的 Microsoft 官方资料表明，Windows Container storage scratch 与这一配额控制属于容器执行模型；客户端容器宿主要求限于 Windows Pro/Enterprise 等受支持配置并要求容器能力。Microsoft 的[容器许可 FAQ](https://learn.microsoft.com/en-us/virtualization/windowscontainers/about/faq) 将 Windows Pro/Enterprise 上的容器用途列为开发或测试；[Windows Container base image 补充许可](https://learn.microsoft.com/en-us/virtualization/windowscontainers/images-eula) 也限制客户端宿主上的该镜像用于开发/测试。

因此，即使 HCS API 能创建有存储上限的 sandbox，也不能据此把它视为普通 Haven 消费级客户端发行版可用的生产后端。容器宿主配置、镜像许可和完整 OS 强制配额验收必须一起成立。

## 决定

- HCS `StorageSandboxSize` 仅保留给明确支持、具备相应容器运行条件和许可的部署目标评估；Windows Server、IoT 或其他部署是否满足条件，必须逐目标核验，不从 Pro/Enterprise 开发功能推导生产授权。
- Haven 不默认安装 Docker/containerd、Windows Container feature 或 Hyper-V，也不下载或内嵌 Windows container base image 来绕过宿主配置和许可边界。
- 对普通 Windows 客户端生产路径，当前没有已接受的 OS 级每执行存储配额后端。`WorkspaceWrite` 下 Shell、Skill、MCP stdio 子进程继续 fail closed，直到找到适用的 OS-backed storage owner 并通过运行期超额写入拒绝验收。
- 不把配额轮询、剩余空间检查、容器退出后扫描或 Job Object I/O 限制当作存储容量硬上限。
- 本 ADR 不选择特权 broker。新增安装/服务权限、操作系统版本范围或系统级配额变更，仍须先作为明确部署决策评估。

## 替代方案

- 把 HCS 当作所有 Windows 10/11 客户端的默认配额后端：拒绝；官方容器支持和许可范围不能支持该默认假设。
- 要求普通用户自行安装 Docker Desktop 或 containerd：拒绝；这仍不能解决客户端容器生产许可，也把容器宿主作为产品运行依赖。
- 不经 OS 后端直接接通 AppContainer Shell 并在超限后终止：拒绝；终止前仍可能耗尽宿主机卷。
- 安装提权 broker、为每次执行创建专用身份并配置系统存储限额：暂不选择；其安装、凭据、IPC 和撤销生命周期需要用户明确接受。

## 影响与迁移

更新 HCS 候选的适用范围，不改变当前授权或执行行为。没有数据库、配置、IPC 或持久数据变化，无需迁移/重置。`WorkspaceWrite` 子进程拒绝是有意的安全边界，不得由后续 UI 或运行时能力探测静默放宽。

## 验证

- Microsoft 官方[资源控制文档](https://learn.microsoft.com/en-us/virtualization/windowscontainers/manage-containers/resource-controls) 将 `StorageSandboxSize` 列为 HCS 的容器存储控制；[容器存储文档](https://learn.microsoft.com/en-us/virtualization/windowscontainers/manage-containers/container-storage) 将运行期 scratch 描述为容器存储模型的一部分。
- [Windows Container 宿主要求](https://learn.microsoft.com/en-us/virtualization/windowscontainers/deploy-containers/system-requirements) 与[客户端准备指南](https://learn.microsoft.com/en-us/virtualization/windowscontainers/quick-start/set-up-environment) 核对了客户端版本/edition 和 Hyper-V 前置条件。
- 本切片只改决策文档；运行期实现仍由 ADR 1209 的 Windows 超额写入负向验收 Gate 阻断。

## 回滚

若 Microsoft 许可或 Haven 的正式支持矩阵确认了客户端生产容器执行的授权依据，可新增 ADR 明确适用 Host OS、镜像来源、许可责任和验收矩阵，再决定是否恢复 HCS 为该范围的后端候选。删除本 ADR 不得自动放开 `WorkspaceWrite` 子进程。
