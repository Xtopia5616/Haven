# ADR 1268：将 Skill adapter 接入实时授权与输出审阅

## 状态

已采纳并实现 Tools、App IPC 与 UI 调用链。管理页 fail-closed 例外已由 [ADR 1326](1326-run-management-skill-previews-as-session-steps.md) 后续决策取代。该实现不表示真实 HCS、guest 配额或目标 Windows 支持矩阵已验收；这些仍是独立发布 Gate。

## 背景

ADR 1265–1267 已建立闭合的 brokered Skill profile、固定 Python runtime、独立只读 Skill 输入和 workspace 输出 review，但模型调用仍被 WorkspaceWrite 策略拒绝。即使接入 HCS，如果执行边界不随当前 sandbox 策略变化、guest 启动不重检 proof、取消/撤权不能终止 attempt，或 review 没有可信工具步骤身份和 UI 入口，也不能安全开放该路径。

## 决定

- Skill 的授权 contract 随 sandbox mode 解析：FullAccess 使用 HostProcess 与 Opaque network；Windows WorkspaceWrite 使用 WorkspaceIsolated 与 NetworkAccess::None，并且必须装配 HCS execution port 和 review owner；ReadOnly 不允许执行。缺少后端或 review owner 时，在授权 contract 阶段 fail closed。
- `ExecutionPolicyAuthorization` 保存 proof 所绑定的 execution boundary。执行前 adapter 同时检查 live sandbox mode 和该 boundary；策略 revision、workspace generation 或授权状态变化会使启动 permit 失效。
- WorkspaceWrite 的模型 Skill 调用只运行固定 HCS Python profile。请求携带独立只读 Skill bundle、有界 JSON 参数、ExecutionWorkspace snapshot、512 MiB quota、执行时限和文件树/输出限制；broker 启动 handshake 前重新获取 live permit，guest 启动确认后释放短期 permit。用户取消、全局策略撤权、工具设置撤权或 session trust 撤权会取消并等待 broker worker 收束。
- 工具执行 owner 把可信 session id 与预先生成的 `step-*` id 注入 `ToolExecutionContext`。review reserve 绑定这两个身份；不从模型参数或 renderer 输入取身份。输出仍遵守 Skill 行数上限和每流 8 MiB 上限，完成的 guest workspace 只进入现有 `SandboxOutputReview` owner。
- review manifest、丢弃和晋升 IPC 只接受当前 session 投影中的精确 `shell` 或 `skill__*` 步骤。UI 的 Skill 结果卡显示输出和同一 review panel；晋升仍由 `files.promote_sandbox_output` 的一次性确认、owner-held manifest index 与现有 Files policy 完成。
- 应用管理页的直接 Skill 执行入口没有 chat step 身份与 review 展示生命周期，因此仍按其 HostProcess contract 在 WorkspaceWrite 下拒绝；它不能绕过模型 Skill adapter 的 HCS/review 链路。

## 拒绝的方案

- 只把 Skill policy 改成 WorkspaceIsolated，却继续调用本机 Python runner：Haven 无法约束它读取或写入 workspace 外路径，拒绝。
- 由模型或 UI 提交 session/step identity：可把输出审阅绑定到其他工具卡片，拒绝。
- 复用 Skill 的无网络 Python profile 执行 MCP stdio server：MCP server 的 runtime、网络与进程树语义不同，不能继承该 profile；stdio MCP 在 WorkspaceWrite 下继续 fail closed。
- 让 Skill 输出自行携带宿主路径或直接落入 ExecutionWorkspace：会绕过有界快照、manifest review 和一次性晋升授权，拒绝。

## 影响、回滚与重置

没有数据库、配置、MCP wire 或 broker protocol 变化；不需要重置持久数据。回滚时从 builtin catalog 移除 HCS adapter/runtime 注入，并恢复仅允许 FullAccess HostProcess 的 Skill contract；review IPC/UI 可以随该能力一并撤回。现有 broker service/client 版本不因本切片改变。

## 验证

- 自动验证覆盖 FullAccess 与 WorkspaceWrite 的 boundary/network policy、缺少 backend/review owner 的 fail-closed 行为，以及 App IPC 只接受精确 Shell/Skill 步骤。
- `cargo fmt --all -- --check`、`cargo check --locked -p haven-tools`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings` 均通过；workspace 测试中 `haven-tools` 为 964 通过、2 忽略。
- App IPC 精确步骤身份测试通过；`scripts/check-ipc-contracts.ps1` 通过，90 个 handler 的注册、生成 TypeScript、安全元数据和 UI/非 UI 处置一致；ADR 索引检查通过。
- `corepack pnpm run check` 通过；UI 全量测试 1,318 项通过。Skill 结果卡仅有文案调整发生在该次 UI 全量测试之后。
- 未启动 Windows Containers/Hyper-V 验收环境；未运行真实 HCS guest、超额写入或支持矩阵验收。
