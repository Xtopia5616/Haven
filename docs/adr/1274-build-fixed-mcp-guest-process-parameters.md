# ADR 1274：构造固定的 MCP guest process parameters

## 状态

已实现受管 package 的闭合 HCS `ProcessParameters` 构造器，以及只能由 startup quota attestation 创建的 Node runtime proof。该构造器尚未连接 broker request handler；MCP guest 不会因此启动，`WorkspaceWrite` 下 stdio MCP 继续 fail closed。

## 背景

ADR 1272 在受保护镜像 catalog 中登记 package ID、版本和 `.mjs` leaf entrypoint；ADR 1273 让 HCS process owner 校验并拥有可选 stdin handle。下一步需要将这两个内部边界合成一份固定 process contract，避免未来 session handler 把 MCP 配置中的宿主 command、参数、环境或工作目录拼进 HCS JSON。

## 决定

- `HcsGuestProcessParameters::from_mcp_package` 只接受已由严格 catalog parser 验证的 `McpNodePackageProfile` 和 `McpNodeRuntimeProof`。该 proof 只能从 broker startup `QuotaRuntimeAttestation` 的 Node runtime bit 取得；请求 JSON、包 ID 或 `SCM Running` 状态不能构造它。缺少 proof 时返回静态 `InvalidInput`，不生成 process parameters。
- guest executable 固定为 `C:\\Haven\\Node\\node.exe`；entrypoint 固定解析到 `C:\\Haven\\McpPackages\\<package_id>\\<entrypoint>`，working directory 固定为该 package 目录。catalog 约束 package ID 与 leaf `.mjs` 名称；该 builder 不接受其它 executable、argv、environment、cwd 或 HCS JSON。
- 进程使用现有最小固定环境和 restricted token，`CreateStdInPipe`、`CreateStdOutPipe`、`CreateStdErrPipe` 全为 true，供 MCP JSON-RPC byte stream 后续接线。没有 ExecutionWorkspace/user path 映射；该 process parameter builder 本身不打开网络，也不改网络 gate。
- 本切片不验证 guest package 文件存在或 digest，不传递当前 MCP 配置的参数/环境/动态 secret，不启动 process，也不代替一次性 process-start permit。下一阶段必须在 process create 前补 package content proof、配置到 package ID 的可信绑定、资源预算、双向流/取消与 generation 生命周期；否则不调用 builder。

## 替代方案

- 使用 MCP config 的 `command` 或 entrypoint：拒绝，会让不可信的宿主配置控制 guest executable selection。
- 让 builder 接受任意 argv、环境表或工作目录：拒绝，敏感值会扩大 IPC/logging 面，且可改变 Node 启动方式。
- 用普通 bool 表示 Node 已通过 startup probe：拒绝，任意 caller 可误传 true；使用 opaque proof 类型限制 minting owner。
- 仅启用 Node 可执行文件而跳过 package profile：拒绝，runtime 与 MCP server 是两个独立身份。

## 影响与回滚

只增补 `haven-platform` 内部 HCS parameter builder 和 proof 类型，不改 MCP 配置、broker wire protocol、数据库、UI 或现有 Shell/Skill 执行。回滚时移除该 builder/proof；目前没有 server process 或用户数据需要清理。

## 验证

- `hcs_process` 单测确认固定 Node executable、catalog package 路径、restricted token、三条标准流 pipe、无 workspace 与无 `NODE_OPTIONS`；无 runtime proof 时拒绝构造。
- 验证通过：`cargo fmt --all -- --check`、`cargo test --locked -p haven-platform`（195 passed、1 ignored）、`cargo clippy --locked -p haven-platform -- -D warnings`。
- 本 ADR 未运行真实 HCS 或 guest MCP server；package 文件完整性、运行期背压/取消和 revoke cleanup 仍未验收。
