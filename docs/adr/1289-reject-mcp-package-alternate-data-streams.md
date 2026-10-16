# ADR 1289：拒绝受管 MCP package 的 NTFS alternate data streams

## 状态

已实现 package tree 扫描器的拒绝规则和 Windows PowerShell 回归测试。真实 Windows Containers/HCS guest 验收仍按既有发布 Gate 暂缓。

## 背景

[ADR 1288](1288-verify-managed-mcp-package-trees.md) 让 catalog v6 摘要覆盖受管 package 根下每个普通文件的相对路径和主数据流字节。NTFS alternate data streams 可通过带流名的文件路径读取，但不会被普通文件流摘要覆盖；若允许它们存在，package 代码可以读取未被 `tree_sha256` 证明的内容。这会削弱“package 根下内容均由 catalog proof 固定”的边界。

## 决定

- 共用扫描器在处理 package 根以及每个文件系统项（普通文件和目录）时，使用 Windows PowerShell `Get-Item -Stream *` 列举数据流。
- 只允许默认 `:$DATA` 流。任何其它命名数据流都会拒绝整个 package tree digest；流枚举失败也按扫描失败处理并关闭该 package proof。
- 每个普通文件以 `.NET SHA256.ComputeHash(Stream)` 流式计算摘要，避免依赖 PowerShell Utility 模块自动加载，也避免将文件整体读入内存。
- 不把 ADS 字节纳入摘要格式，不改变 catalog v6、package marker 或 broker protocol。管理员摘要工具和启动 probe 仍共用同一函数，因此管理员无法为包含 ADS 的 package 生成可登记摘要。
- 受管 package 根必须位于支持 Windows alternate-stream 枚举的文件系统；不支持流枚举的卷按 fail-closed 处理。

## 替代方案

- 将 ADS 纳入 digest：拒绝。需要定义默认流与命名流的稳定规范化、排序和路径编码，扩大 catalog 算法面；当前 package 不需要 ADS，拒绝它们更简单。
- 继续允许 ADS 并保留 ADR 1288 的有限声明：拒绝。受管 package proof 用于运行可执行的第三方代码，未摘要的数据流仍可改变其可读运行资源。

## 影响与回滚

catalog 格式、用户配置、数据库和 MCP wire protocol 均不变，无需重置数据。已有包含 ADS 的管理员 package 会在下次 digest 生成或 broker 启动 probe 时 fail closed；管理员应清理非默认流后重新计算摘要。回滚时恢复扫描器允许 ADS 的旧行为，但不得通过兼容性开关使未证明 package 获得运行 proof。

## 验证

- Windows 回归测试验证普通多文件 package 的摘要与独立 canonical fixture 一致，再向 entrypoint 添加 NTFS ADS 并验证同一函数拒绝 package；测试通过。
- `cargo test --locked -p haven-platform`：220 passed、0 failed、1 ignored（需要 elevated HCS storage formatting privilege）。严格 Clippy、`cargo fmt --all -- --check`、ADR 索引与 `git diff --check` 均通过。
- 本轮不运行真实 HCS、guest 超额写入或目标镜像验收；这些仍是既有发布 Gate。
