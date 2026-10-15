# ADR 1195：分离项目执行工作区与 OS sandbox 工作区

## 状态

目标架构已采纳；生产 launcher、SandboxWorkspace 与变更晋升仍待实施。

## 背景

`ExecutionWorkspace` 是 Haven 的 canonical 项目根，授权、Files 相对路径和 Shell 默认目录以它的 generation 为边界。ADR 1194 的 Windows 探针证明，AppContainer 写入目录除了 package SID 的 DACL 许可，还需要兼容低完整性进程的 mandatory label。

直接给用户项目根添加低完整性标签会改变其它低完整性进程对这些文件的写权限；把 AppContainer 的 workspace 文件复制回项目根也会绕过 `writable_roots`、工具 `allowed_paths`、授权 proof 和目录句柄。OS sandbox 的可写根因此不能与 canonical 项目根复用。

## 决定

- 保留 `ExecutionWorkspace` 作为 canonical 项目根和 Haven 授权根。AppContainer、Skill、Shell 或 MCP launcher 不得为满足低完整性写入而更改该根或其现有内容的 mandatory label。
- 引入独立的 `SandboxWorkspace` 概念，作为 OS sandbox 的私有低完整性工作根。它由后端 owner 创建和回收，与 UI 导航 workspace 无关；绑定创建时捕获的 `ExecutionWorkspace.generation`、安全策略 revision 和执行 owner/lifecycle。任何绑定失效时，旧 sandbox 不能复用于新 attempt。
- sandbox 输入是固定 generation 的只读文件快照；复制过程拒绝 reparse point、非本地路径和超出资源上限的条目，不使用会共享源文件安全描述符的 hard link。sandbox 内的全部修改先留在私有根中。
- sandbox 输出必须成为有界、可审阅的变更清单。每个晋升操作回到现有 AuthorizationEngine 和 `DirectoryCapability` 写路径，重新检查当前 policy revision、workspace generation、`writable_roots` 与 tool `allowed_paths`；禁止把 scratch 目录递归复制到项目根，也禁止绕过原有确认契约。
- AppContainer 的网络 capability 由工具操作契约与当前 NetworkPolicy 决定。`Deny` 不授予 capability；`Ask` 的确认必须明确说明后代进程获得的网络范围；`Restricted` 不能近似为广域 `internetClient` capability，缺少进程级目的地强制时继续 fail closed。运行中的 sandbox 不因配置更改而扩大权限。
- Windows Job Object 继续只管理进程树生命周期与数量上限；filtered environment 继续管理环境继承。两者都不取代 AppContainer、SandboxWorkspace 或变更晋升授权。
- 本 ADR 不重新定义 `FullAccess` 为 OS sandbox。生产接线前必须完成 `SandboxWorkspace` 的容量/配额、snapshot pinning、MCP 长生命周期存储、后台 ToolRun 取消和 workspace switch 生命周期契约，并实现原生子进程标准流与显式 handle-list 的安全传递。

## 替代方案

- 将 `ExecutionWorkspace` 直接降为低完整性：拒绝，会削弱用户项目文件原有的完整性边界，并让同一用户的其它低完整性进程也可能写入。
- 只在 canonical root 上增加 package SID DACL：拒绝，低完整性进程仍会被 Mandatory Integrity Control 阻止写入；放宽 label 才能写，又产生上一项风险。
- 进程退出后把 SandboxWorkspace 整体复制回项目根：拒绝，会越过逐路径授权、确认和当前 workspace generation 检查。
- 用通用 AppContainer network capability 实现 `Restricted`：拒绝，宽域 capability 不能表达 Haven 的目的地校验与实时策略边界。
- 继续将当前 Job Object、环境过滤或 `cwd` 描述为 OS sandbox：拒绝，它们分别解决生命周期、环境继承和工作目录问题，不提供文件/网络访问控制。

## 影响与迁移

本 ADR 仅确定后续实现契约，不改变当前 `ExecutionWorkspace`、`SandboxMode`、`NetworkPolicy`、配置/IPC、数据库或生产子进程行为，无需重置。WorkspaceWrite 下 opaque 子进程仍按 ADR 1181 拒绝；FullAccess 子进程仍遵循当前 Windows 用户 OS 权限，直至新的隔离链路和用户可见切换规则完成。

## 验证

- ADR 1194 的 Windows 真实进程探针验证 package SID DACL、低完整性 label 与无网络 capability 的组合效果；它只改动并恢复临时 fixture，不触碰项目工作区。
- 后续实现必须分别验证源 workspace label/内容未改变、snapshot 拒绝 reparse/path escape 和容量越界、scratch 输出边界、proof/revision/generation 失效、逐路径晋升拒绝、进程后代回收、网络 capability 映射与过滤环境下的标准流传递。
- 在这些端到端门禁通过前，不得开放 WorkspaceWrite 下的 Shell、Skill 或 MCP stdio 子进程。

## 回滚

还原本文与路线图/ADR 索引中的目标契约说明。无运行时配置或持久状态需要回滚。
