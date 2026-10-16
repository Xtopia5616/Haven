# ADR 1272：在镜像 catalog 中登记受管 MCP package

## 状态

已实现受信 package identity 元数据；尚未验证 guest package 内容，也未接入 MCP server 启动。`WorkspaceWrite` 下 stdio MCP 继续 fail closed。

## 背景

ADR 1271 固定了 broker 可使用的 Node 24.21.0 runtime，但没有定义受信任的 MCP package identity。现有 MCP 配置仍含宿主 `command`、参数、环境和工作目录，不能直接转换为 guest executable 或任意 Node 参数。broker 需要先有一个不接受调用方路径/命令的管理员受管映射，后续才能把配置绑定到明确的 package ID。

## 决定

- 镜像 catalog 升为 v4，增加至多 64 条 `mcp_node_packages`。每项包含小写 package ID、严格 `major.minor.patch` 版本和单个 `.mjs` entrypoint 文件名；profile 不包含 command、host path、工作目录、环境、参数或 HCS 配置。
- package ID 限制为 1–64 个小写 ASCII 字母、数字或连字符，并以小写字母开头；版本组件不允许前导零且最多 9 位；entrypoint 只能是固定 package 目录下 128 字节以内的 leaf `.mjs` 名称，拒绝路径分隔符、Windows 设备名及其它扩展名。
- 至少登记一个 package 时必须同时配置 ADR 1271 的固定 `haven-mcp-node-v1` runtime。catalog 拒绝重复 ID、未知字段、无效 descriptor 和超过数量上限的列表。catalog 当前仍从受保护的 broker 安装目录读取；请求不能选择 catalog 文件或 layer。
- 这些字段是管理员批准的 package identity mapping，不是 package 内容证明。此 ADR 不验证 guest 文件存在/摘要，不创建 process，亦不把现有 MCP config 的 `command` 映射到 package ID。后续 launch gate 必须增加内容/entrypoint 验证和可信配置绑定，不能只因 catalog 有条目就启动。
- 旧 catalog v3 被严格拒绝；按测试版本 reset/reprovision broker 安装目录时使用 v4。MCP 用户配置、数据库、Tools authorization 与执行行为均不变。

## 替代方案

- 把 `command`、cwd 或任意 entrypoint 从 MCP 请求直接送入 guest：拒绝，会把配置路径变为 HCS process selection。
- 用 Node runtime profile 本身代表 MCP package：拒绝，runtime 与 server package 是独立身份。
- catalog 暴露任意脚本路径或命令行：拒绝，固定 ID/单文件入口让下一阶段能实现封闭映射。
- 在本切片同时开启 server launch：拒绝。当前没有 package 文件/内容证明、双向 session stream、网络边界与 generation 撤销。

## 影响、配置与回滚

broker 管理员 provision 的 `catalog.json` schema 从 v3 升至 v4；不涉及用户配置、数据库或用户目录迁移。管理员只可登记实际 baked into protected image 的 package；当前代码不会启动它们。回滚时恢复 v3 catalog/parser 并移除该元数据；本切片没有创建持久运行态。

## 验证

- `haven-platform` image-catalog 单测覆盖合法 ID 映射、缺少 Node runtime、重复/超量 package、路径、Windows 设备名、版本、未知字段和扩展名拒绝。
- 验证通过：`cargo fmt --all -- --check`、`cargo test --locked -p haven-platform`（191 passed、1 ignored）、`cargo clippy --locked -p haven-platform -- -D warnings`、`pwsh -NoProfile -File scripts/check-adr-index.ps1`。
- 后续 package launch 切片仍需 guest 文件存在性/内容验证、参数与 environment 白名单、双向流容量与取消、generation revoke/cleanup 以及真实 Windows/HCS 验收；本 ADR 未运行 guest。
