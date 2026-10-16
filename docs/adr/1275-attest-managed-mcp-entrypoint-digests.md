# ADR 1275：证明受管 MCP entrypoint 摘要

## 状态

已实现 catalog v5 entrypoint SHA-256、启动 probe 的独立 package 身份结果和绑定到具体 catalog profile 的 opaque proof。broker request handler 尚未使用该 proof；stdio MCP 在 `WorkspaceWrite` 下仍 fail closed。

## 背景

ADR 1272 将管理员受管 MCP package 映射到固定 guest `.mjs` entrypoint，但此前 catalog 只描述名称，不能证明 guest 中实际执行的字节仍与管理员登记内容一致。ADR 1274 的 proof 只代表 Node runtime 已通过启动验证，不能单独授权启动任意 package。

## 决定

- 镜像 catalog 升为 v5。每个受管 package 必须提供 64 位小写十六进制 `entrypoint_sha256`；缺失、格式不符或未知字段均拒绝加载。每个 catalog 最多登记 16 个 MCP package，使固定 probe 在最大 ID/entrypoint 长度下仍满足 Windows 命令行上限。
- 启动期固定 quota probe 按 catalog 提供的 package ID、leaf entrypoint 和摘要检查 `C:\Haven\McpPackages\<id>\<entrypoint>`。profile 字段受到严格 ASCII 校验，再作为固定字段表送入 guest 中的固定 PowerShell 循环；请求方不能控制路径或命令。仅验证 entrypoint 文件字节摘要，不声称验证 package 的依赖、资源或整个目录树。
- quota marker 升为 V4，package 结果以独立的 `HAVEN_MCP_PACKAGE_ATTESTED_V1` 行返回。quota、Skill Python、Node runtime 与 package entrypoint 是独立 gate；package 行缺失、畸形、重复或报告未知 ID 时不授予任何 package proof，但不关闭已验证的 quota/Node/Skill gate。entrypoint 摘要不匹配只关闭该 package 的证明。
- `QuotaRuntimeAttestation` 保存通过验证的 package ID。它仅在 Node runtime 已通过验证且 package ID 对应本进程所持的受保护 catalog profile 时，才能创建 `McpNodePackageProof`。MCP HCS process builder 现在只接受该 proof，并从 proof 内的 catalog profile 取固定 executable、entrypoint 和工作目录；不再接受独立 Node bool 与可替换 package profile。
- probe stdout 上限为 4 KiB。package 描述使用有界字段表，避免按每个条目扩展 PowerShell 脚本并突破 HCS 命令行长度限制。
- 此切片不将 MCP 配置绑定到 package ID、不启动 guest server，也不传递动态凭据或用户 workspace。双向字节流、背压/取消、generation 撤权和有界关闭仍需后续实现；在这些边界接通前不能消费 package proof。
- 旧 catalog v4 被拒绝；测试/部署需按项目重置流程重新 provision v5 catalog。MCP 用户配置与持久数据 schema 不变。

## 替代方案

- 只验证 Node runtime：拒绝，Node 可执行文件的身份不能证明将运行的 MCP server package。
- 只校验 package ID/entrypoint 元数据：拒绝，guest 文件内容可能与受保护 catalog 的身份声明不符。
- 由调用方传入 entrypoint 路径或 SHA-256：拒绝，这会让不可信配置控制执行目标或信任根。
- 将任何 package marker 错误作为 quota probe 整体失败：拒绝，package 身份与磁盘 quota 是独立能力门，单 package 部署问题不应错误改变 Shell/Skill 的 quota 状态。
- 校验整个 package 目录：本切片不采用；当前闭合执行契约只启动单个 leaf entrypoint，完整依赖/资源清单需要独立的 bundle manifest 与验收设计。

## 影响与回滚

只调整 `haven-platform` 的受保护 catalog schema、固定启动 probe 及尚未接线的 process-parameter proof，并新增 ADR。没有改 broker wire protocol、MCP 用户配置、数据库、UI 或实际执行可达性。回滚需同步移除 entrypoint proof 并重新部署 catalog；旧 v4 catalog 不兼容 v5，按 release/reset 流程重新 provision。

## 验证

- 单测覆盖 digest 格式、固定 entrypoint 路径和摘要、最大 catalog 列表的命令行长度，以及 package marker 缺失/畸形/重复/未知 ID 不关闭 quota gate。
- 验证通过：`cargo fmt --all -- --check`、`cargo test --locked -p haven-platform`、`cargo clippy --locked -p haven-platform -- -D warnings`、`pwsh -NoProfile -File scripts/check-adr-index.ps1`。
- 未运行真实 HCS 或 guest 超额写入验收；package proof 尚未接入 server 启动，entrypoint 以外的 package 依赖内容也未校验。
