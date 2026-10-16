# ADR 1287：将工作区绑定的 MCP 调用隔离为单次 guest

## 状态

已接受，分阶段实现；本 ADR 的应用端调用链和 UI 关联已完成。Tools MCP adapter 从 proof-bound 上下文取得 workspace capability；`WorkspaceWrite` stdio 要求规范 session/step 身份，并在策略撤销时取消 MCP 调用。每次调用创建新 MCP client/broker generation，完成协议发现、恰好一次 `tools/call`，等待 guest 停止确认后才返回。MCP broker protocol v12 在固定限额的 workspace snapshot（最多 10,000 entries、深度 32、单文件 64 MiB、总量 128 MiB、传输 60 秒）基础上，增加停止后的 writable-layer 差异导出、客户端重建与 manifest 复核，以及清理前 ACK。Direct MCP review 已进入 `SandboxOutputReviewOwner` 并由 MCP 结果组件按 session/tool step 展示；CodeMode/Workflow 子调用的 review 身份现由 Agent bridge 校验为本次生成的 canonical child `step-*` 后，通过 UI-only ToolResult envelope 传给父结果卡片，分别按 child step 查询同一 review owner。子 ID 不进入 CodeMode 脚本输出、Workflow observation 或结构化模型结果；前端只接受至多 64 个 canonical step IDs。审阅可逐文件走既有晋升授权，输出不会自动写回项目。既有 session event 中的可选 review 身份只是一条查询关联：owner 到期或重启后丢弃时，查询为空，不恢复 review 内容或晋升权。真实 HCS、quota 超额写入和目标机验收继续作为发布 Gate，按用户要求不执行。

## 背景

ADR 1286 将固定 ManagedPackage stdio generation 接入 Windows HCS broker，但没有挂载 `ExecutionWorkspace`。现有 stdio generation 会跨多个 MCP 请求保持运行，因此在启动时挂入一个工作区副本并不能实现“每次工具调用使用当前 workspace snapshot”。在存活 guest 上原地刷新文件还会让并发调用、进程缓存和上次调用写入互相影响，无法形成可审阅的单次执行边界。

Tools 已在 proof-verified `ToolExecutionContext` 中保存可信 `session_id`、`step_id`、workspace generation 和策略授权；`SandboxOutputReviewOwner` 已提供按 session/step 绑定、容量受限且可供用户逐文件提升的 review 生命周期。HCS Shell 路径已有有界 workspace 复制、guest 输出差异传输、停止证明和 quota 校验，可供新调用路径复用。

## 决定

- **项目文件型 ManagedPackage 调用按 MCP tool step 创建一次性 guest。** `WorkspaceWrite` 使用新 MCP client generation、协议发现、恰好一次 `tools/call`、固定上限快照传输、guest 只读挂载和停止确认；停止后还须通过 quota 检查、writable layer bounded output diff、客户端重建与 manifest 复核，之后才交给 review owner。下一次工具调用必须从新的 snapshot 和空 guest 层开始。不得在长驻 generation 中原地更新 workspace，也不得复用前一次调用的 writable layer。
- **MCP adapter 只从 proof-verified Tools 上下文取得 session、step 和 workspace capability。** session/step ID 必须用统一 `is_canonical_id` 验证；模型参数、MCP 参数、配置字符串或 renderer 不能提供这些身份。MCP crate 接收不含绝对路径的 typed workspace capability，不负责授权，也不能获得任意宿主路径。
- **CodeMode/Workflow 的每个嵌套 MCP 调用需要自己的 canonical child step ID。** Agent bridge 为每次调用生成独立 `step-*` 并与 authorization proof 一同传入 Tools execution boundary；不得复用父 tool step 或 Workflow 中模型可见的 step label。snapshot、输出差异、review owner 与父结果卡片关联均按每个 child invocation 单独处理。Agent bridge 只接受 Tools result 中与本次生成 ID 完全相同的 review 身份；结果 envelope 字段只供 UI 查询，不得进入模型可见的子工具输出。
- **UI 关联使用受限的 side channel。** MCP adapter 在 review owner 成功 retain 后把 proof-bound `step_id` 写入 runtime-only `ToolSignals`；CodeMode/Workflow 组合工具收集 child IDs，并只投影到 `ToolResultEnvelope.sandbox_output_review_step_ids`。字段缺省时沿用旧 envelope；空列表不序列化；前端拒绝非 canonical ID 或超过 64 项。review 内容仍只由 `SandboxOutputReviewOwner` 查询，UI 身份不包含路径、manifest 或晋升授权。
- **沿用现有授权与输出 owner。** 工具授权、MCP target binding、实时 sandbox/policy revision 和 process-start permit 都须在副作用前验证；policy permit 覆盖整次调用和 guest 收束。workspace 输入、输出文件数/字节数/深度、stdio 和单次执行时间均受硬上限约束。只在 HCS stop proof、quota 与输出 diff 验证全部成功后，才把 `SandboxOutputReview` 交给 Tools owner，并以 `(session_id, step_id)` 保留；结果不得自动写回 workspace。
- **失败时 fail closed。** 缺少可信 session/step、workspace capability、review owner、有效授权、broker/quota proof、停止证明或有界输出审阅时，不执行或不接受输出。guest 是否已完成外部 MCP 副作用无法确定时保留 `TimedOutUnknown`/未知结果语义；禁止自动重试非幂等调用。失败或取消不得留下可复用 guest 或半成品 review。
- **一次性调用的服务状态必须明确。** ManagedPackage 工具调用以新的 MCP 协议 generation 执行，服务状态不能依赖进程内 session memory；需要跨调用状态的服务必须把它放在 MCP 服务自身管理的外部存储中。工具目录可以继续由已绑定配置 generation 的缓存提供，但运行时调用不能重定向到重连后的 package 配置。
- `McpSessionOpen` 携带可选、服务端固定校验的 workspace copy profile；attach 后 broker 单独请求 bounded snapshot，客户端只传树条目和文件内容。停止后的差异流只发送受限相对路径、change manifest 与有界文件块；客户端重建最终树并重算 manifest，显式 ACK 后 broker 才清理 attempt。该 wire contract 升至 broker protocol v12。HostProcess 行为不变；绝不传输宿主绝对路径、任意命令、用户参数或环境变量作为 package 启动指令。
- **真实环境验收范围保持独立。** 本地契约测试可以验证身份、上限、拒绝路径和状态机；Windows 容器 quota proof 与 guest 超额写入仍未验证，发布前必须由受支持的 Windows Containers/HCS 环境完成。

## 替代方案

- 在长驻 guest 内按调用覆盖 workspace：拒绝。并发、残留文件和进程缓存使调用间隔离无法证明。
- 把 workspace 路径直接交给 MCP server 或命令行：拒绝。它会越过 broker 的受限文件树传输与固定 package catalog。
- MCP 完成后自动把文件复制回 workspace：拒绝。模型调用结果不能自行批准宿主文件写入；输出必须进入既有 review/promotion 流程。
- 继续拒绝所有需要项目文件的 ManagedPackage：拒绝。改为仅在固定 profile、授权 workspace snapshot、HCS stop/quota proof 与 bounded output review 全部成立时开放 `WorkspaceWrite`；缺少任一条件仍 fail closed。

## 影响与回滚

workspace snapshot 仅限新的 `WorkspaceWrite` 单次 ManagedPackage generation，不改变数据库或 MCP 配置格式；协议 v10/v11 broker 与 v12 client/server 不互通，升级必须成对部署。guest writable layer 在 stop proof 后随 attempt 清理；客户端重建的输出树由 Tools review owner 临时持有，不会自动写入用户项目。输出审阅切片可独立回滚到当前只读 snapshot 行为。既有 MCP config 的 `ManagedPackage { package_id }` 无需增加用户命令或宿主路径字段。

## 验收条件

- 每次调用在新的 guest 与 workspace snapshot 中执行；broker 与客户端分别验证 workspace 复制上限；同一 server 的并发调用不会共享 writable layer。
- 真实调用身份来自 Tools proof context，缺失或伪造身份时不会启动 guest。
- Direct、CodeMode 和 Workflow 的每个 MCP child invocation 都使用各自的 canonical step identity；Direct review UI 按 session/tool step 展示 review，CodeMode/Workflow 父卡片按 Agent bridge 认证的 child step identities 展示每份 review。模型可见 CodeMode/Workflow 输出不包含这些身份。
- 输入复制和输出 diff 的路径、数量、字节、深度和时间上限在应用与 broker 边界分别校验；输出传输有独立 60 秒期限、逐文件 SHA-256、完整 manifest 重扫与接收端重建校验。
- guest 未停止、quota 未证明、输出传输不完整、策略撤销和用户取消均不产生可提升 review；外部副作用结果不确定时不会自动重放。
- 成功结果产生仅绑定该 MCP tool step 的 review；Direct MCP review 已接入 Tools owner，Files promotion 仍要求单独的实时授权和用户选择。
- CodeMode 与 Workflow 的父结果 envelope 仅携带受限 child step identity；事件旧格式缺少该字段时解码为空，空值仍不改变旧序列化形状；session 恢复后若 review owner 已过期，UI 查询为空，不能凭身份重建文件或晋升能力。
- 受支持 Windows 容器环境完成 quota probe、超额写入与清理验收后，才能关闭发布 Gate。

## 验证

- `cargo fmt --all`
- `cargo test --workspace --locked`（Agent 671 passed、1 ignored；MCP 113 passed；Tools 972 passed、2 ignored；MCP integration 7 passed；workspace 其余测试与 doc tests passed）
- `cargo test --locked -p haven-agent sandbox_review_side_channel`（1 passed）
- `cargo clippy --workspace --locked -- -D warnings`
- `corepack pnpm run check`（0 errors、0 warnings）
- `corepack pnpm run test:run`（160 files、1,322 tests passed）
- `corepack pnpm run build`（passed）
- `pwsh -NoProfile -File scripts/check-ipc-events.ps1`（36 event channels passed）
- `pwsh -NoProfile -File scripts/check-ipc-contracts.ps1`（90 IPC handlers and generated contracts passed）
- 本切片的 Rust/UI 针对性回归验证：CodeMode、Workflow 和 Common envelope 均覆盖 canonical ID 过滤、64 项上限以及 review 身份不进入模型输出；IPC event 检查覆盖 Common/TypeScript 字段与运行时校验。
- 真实 HCS、quota 超额写入与目标机验收按用户要求暂缓，仍是发布 Gate。
