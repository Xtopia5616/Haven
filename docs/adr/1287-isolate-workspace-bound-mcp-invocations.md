# ADR 1287：将工作区绑定的 MCP 调用隔离为单次 guest

## 状态

已接受，实施待开始。本文冻结目标边界；当前 MCP broker 仍只提供长驻 stdio generation，不具备本 ADR 定义的工作区调用能力。真实 HCS、quota 超额写入和目标机验收继续作为发布 Gate，按用户要求本轮不执行。

## 背景

ADR 1286 将固定 ManagedPackage stdio generation 接入 Windows HCS broker，但没有挂载 `ExecutionWorkspace`。现有 stdio generation 会跨多个 MCP 请求保持运行，因此在启动时挂入一个工作区副本并不能实现“每次工具调用使用当前 workspace snapshot”。在存活 guest 上原地刷新文件还会让并发调用、进程缓存和上次调用写入互相影响，无法形成可审阅的单次执行边界。

Tools 已在 proof-verified `ToolExecutionContext` 中保存可信 `session_id`、`step_id`、workspace generation 和策略授权；`SandboxOutputReviewOwner` 已提供按 session/step 绑定、容量受限且可供用户逐文件提升的 review 生命周期。HCS Shell 路径已有有界 workspace 复制、guest 输出差异传输、停止证明和 quota 校验，可供新调用路径复用。

## 决定

- **项目文件型 ManagedPackage 调用按 MCP tool step 创建一次性 guest。** 调用顺序为新 generation 启动、MCP initialize、执行恰好一次 `tools/call`、关闭 stdio、取得 HCS system stop proof、验证 storage quota、接收 bounded output diff，最后销毁 guest。下一次工具调用从新的 snapshot 和空 guest 层开始。不得在长驻 generation 中原地更新 workspace，也不得复用前一次调用的 writable layer。
- **MCP adapter 只从 proof-verified Tools 上下文取得 session、step 和 workspace capability。** session/step ID 必须用统一 `is_canonical_id` 验证；模型参数、MCP 参数、配置字符串或 renderer 不能提供这些身份。MCP crate 接收不含绝对路径的 typed workspace capability，不负责授权，也不能获得任意宿主路径。
- **沿用现有授权与输出 owner。** 工具授权、MCP target binding、实时 sandbox/policy revision 和 process-start permit 都须在副作用前验证；policy permit 覆盖整次调用和 guest 收束。workspace 输入、输出文件数/字节数/深度、stdio 和单次执行时间均受硬上限约束。只在 HCS stop proof、quota 与输出 diff 验证全部成功后，才把 `SandboxOutputReview` 交给 Tools owner，并以 `(session_id, step_id)` 保留；结果不得自动写回 workspace。
- **失败时 fail closed。** 缺少可信 session/step、workspace capability、review owner、有效授权、broker/quota proof、停止证明或有界输出审阅时，不执行或不接受输出。guest 是否已完成外部 MCP 副作用无法确定时保留 `TimedOutUnknown`/未知结果语义；禁止自动重试非幂等调用。失败或取消不得留下可复用 guest 或半成品 review。
- **一次性调用的服务状态必须明确。** ManagedPackage 工具调用以新的 MCP 协议 generation 执行，服务状态不能依赖进程内 session memory；需要跨调用状态的服务必须把它放在 MCP 服务自身管理的外部存储中。工具目录可以继续由已绑定配置 generation 的缓存提供，但运行时调用不能重定向到重连后的 package 配置。
- 现有 `McpSessionOpen`/session pipe 语义不能被假装成工作区传输。实现需要新的 typed broker 操作与 bounded transfer；协议版本与 HostProcess 既有行为按实际改动同步升级。绝不传输宿主绝对路径、任意命令、用户参数或环境变量作为 package 启动指令。
- **真实环境验收范围保持独立。** 本地契约测试可以验证身份、上限、拒绝路径和状态机；Windows 容器 quota proof 与 guest 超额写入仍未验证，发布前必须由受支持的 Windows Containers/HCS 环境完成。

## 替代方案

- 在长驻 guest 内按调用覆盖 workspace：拒绝。并发、残留文件和进程缓存使调用间隔离无法证明。
- 把 workspace 路径直接交给 MCP server 或命令行：拒绝。它会越过 broker 的受限文件树传输与固定 package catalog。
- MCP 完成后自动把文件复制回 workspace：拒绝。模型调用结果不能自行批准宿主文件写入；输出必须进入既有 review/promotion 流程。
- 继续拒绝所有需要项目文件的 ManagedPackage：作为当前实现的 fail-closed 行为保留，直到一次性 guest 和 review 流程完成；不把拒绝包装成支持能力。

## 影响与回滚

本 ADR 不直接改变代码、配置、数据库或当前运行行为，不要求数据重置。实现完成前，项目文件依赖型 ManagedPackage 仍不可用。实现切片可分别回滚到 ADR 1286 的无 workspace broker session；任何协议版本变更都必须与 app client 和 Windows broker 成对回滚。既有 MCP config 的 `ManagedPackage { package_id }` 无需增加用户命令或宿主路径字段。

## 验收条件

- 每次调用在新的 guest 与 workspace snapshot 中执行；同一 server 的并发调用不会共享 writable layer。
- 真实调用身份来自 Tools proof context，缺失或伪造身份时不会启动 guest。
- 输入复制和输出 diff 的路径、数量、字节、深度和时间上限在应用与 broker 边界分别校验。
- guest 未停止、quota 未证明、输出传输不完整、策略撤销和用户取消均不产生可提升 review；外部副作用结果不确定时不会自动重放。
- 成功结果产生仅绑定该 MCP tool step 的 review；Files promotion 仍要求单独的实时授权和用户选择。
- 受支持 Windows 容器环境完成 quota probe、超额写入与清理验收后，才能关闭发布 Gate。

## 验证

- 当前仅通过代码审查确认所需的 proof context、review owner、workspace snapshot 和 broker 输出传输边界已存在。
- 本轮没有修改实现，也没有运行测试、真实 HCS 或 quota 验收。
