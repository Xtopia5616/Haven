# ADR 1271：固定并证明 brokered MCP Node runtime

## 状态

已实现 runtime identity 与启动期证明。此 ADR 不创建 MCP server process；`WorkspaceWrite` 下 stdio MCP 继续 fail closed。

## 背景

ADR 1269 将 brokered MCP 的 runtime 与 package profile 限定为 broker 安装时固定的受管身份。ADR 1270 已抽出 stdio process launcher 边界，但现有容器镜像目录只固定 Windows layers 和可选 Python Skill runtime，没有独立的 MCP runtime identity。若直接使用 MCP 配置中的 `command`、宿主路径或运行时版本，将把不可信配置带进 HCS guest executable 选择。

项目固定的 Node 工具链版本为 `.node-version` 中的 `24.21.0`。本切片据此提供第一个固定 MCP runtime profile，并复用启动期 quota probe 的 guest binary hash/version attestation；MCP runtime 的失败状态与 Shell quota、Skill Python runtime 分开报告。

## 决定

- 镜像 catalog 升为 v3，新增可选 `mcp_node_runtime` descriptor。它只接受 profile `haven-mcp-node-v1`、精确版本 `24.21.0` 和小写 64 位 SHA-256；未知字段、其它 profile/version、无效 digest 及旧版 catalog 均拒绝。catalog 不接受 executable path、command、entrypoint 或 environment。
- guest runtime 路径固定为 `C:\Haven\Node\node.exe`。quota probe 仅在 descriptor 存在时调用该固定文件的 `--version` 并计算文件 SHA-256；版本与 digest 都匹配才在本次 broker 进程的 attestation 中记录 MCP Node runtime 已验证。probe 不运行 MCP package、用户 command 或模型提供的参数。
- quota probe marker 升为 V3，同时返回写入配额证据、Skill Python runtime bit 和 MCP Node runtime bit。解析器要求字段数量与值精确匹配；未配置 runtime 不得报告 attested。runtime 不匹配只关闭对应 runtime gate，不推翻独立验证成功的 storage quota 证明。
- 认证 execution-gate status 增加 `mcp_node_runtime` 的 `not_configured`、`attested`、`unattested` 状态，broker wire protocol 升为 v8。broker CLI 单独显示 MCP Node runtime 状态；它不表示 MCP process 已启动或获准访问网络/workspace。
- profile 目前只证明固定 runtime binary 的身份，不证明任意 MCP package 的来源、依赖闭包、entrypoint、参数、环境或资源配额。尚无完整 package profile、双向 HCS session stream、网络 gate 或 server-generation revoke path；因此该证明不改变 Tools authorization，且不能作为 `WorkspaceWrite` 下启动 stdio server 的许可。

## 替代方案

- 复用 Skill Python profile：拒绝。MCP server 需要独立的 Node runtime，不能把一次性无网络 Python Skill contract 当作通用 server 进程身份。
- 从请求读取 Node 路径、版本或 digest：拒绝。模型或用户配置不得选择 broker 的 guest executable。
- 缺少或不匹配 Node 时关闭 Shell quota gate：拒绝。Shell storage enforcement 与 MCP runtime 是独立 capability。
- 在本切片直接开放 MCP server spawn：拒绝。仅验证 Node binary 不能证明 package、流背压、长期资源限制、网络隔离及撤权回收已闭合。

## 影响、配置与回滚

管理员 provision 的 `catalog.json` 从 v2 升为 v3，旧版文件由 broker 在启动时拒绝；可选 Node descriptor 缺失时 Shell 与 Skill 现有 gate 仍按各自证明状态工作，MCP Node runtime 显示未配置。该 catalog 位于受保护 Program Files 镜像目录，不是 Haven 用户配置或数据库。本切片不改 MCP server configuration，不迁移用户数据。

回滚时恢复 catalog v2 与 quota marker V2/status protocol v7，并移除 MCP Node runtime status；已经由 v3 证明的 runtime 不会被 MCP 工具使用，因为此版本尚未接通 process launch。

## 验证

- `haven-platform` 单测覆盖 catalog v3 descriptor 严格解析、固定 profile/version/digest、quota marker 精确字段、未配置 runtime 伪造证明拒绝、runtime mismatch 与 quota gate 相互独立，以及 status wire v8。
- 固定 guest probe 参数测试确认 Node 可执行路径、版本和 SHA-256 来自闭合常量与受保护 catalog descriptor；不存在 descriptor 时脚本不包含 Node executable path。
- 验证通过：`cargo fmt --all -- --check`、`cargo test --locked -p haven-platform`（186 passed、1 ignored）、`cargo clippy --locked -p haven-platform -- -D warnings`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`。
- 本轮只运行自动化单测和 HCS 参数生成检查；没有启动真实 HCS、Node guest process、MCP package 或配额超额写入。真实镜像、服务 token、Windows build 与 runtime probe 仍属于既有发布验收 Gate。
