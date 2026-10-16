# ADR 1288：证明完整受管 MCP package 文件树

## 状态

catalog v6、受限 guest 文件树探针、标准 Node 模块搜索路径检查和本机 digest 生成脚本已实现。真实 Windows Containers/HCS guest 验收继续作为发布 Gate；按用户要求，本 ADR 不运行该验收。

## 背景

[ADR 1275](1275-attest-managed-mcp-entrypoint-digests.md) 的 catalog v5 只验证 MCP package 的 .mjs entrypoint。Node 在运行时还会读取 package 内的依赖、配置和资源；只证明入口字节会允许其它 package 文件在登记后被替换而不改变 proof。Node 24 的 [模块文档](https://nodejs.org/download/release/latest-v24.x/docs/api/modules.html)记载 bare package 会逐级查找父目录 node_modules，并可能查找 global folders，因此单独哈希 package 目录不足以覆盖这些外部查找位置。

## 决定

- catalog 升为 v6。受管 package 使用 tree_sha256 替代 entrypoint_sha256；v5 被拒绝，不保留兼容解析。
- package tree digest 的唯一算法为：
  1. 递归枚举固定 guest 根 C:\Haven\McpPackages\<id> 下的普通文件；拒绝 reparse point、路径越界、大小写不敏感的重复相对路径、控制/Win32 非法路径段，以及不严格 UTF-8 的路径。
  2. 路径相对于 package 根，分隔符统一为 /，保留实际大小写。每个文件生成 relative_path + NUL + lowercase_hex(SHA256(file_bytes)) 记录；按 OrdinalIgnoreCase 排序后，每条记录末尾追加 LF，以无 BOM 严格 UTF-8 编码，再计算其 SHA-256 作为 tree_sha256。entrypoint 必须是该文件集合中的 leaf .mjs。
  3. 为限制启动 probe 的 CPU、内存和磁盘读取，每个 package 最多 8,192 个文件系统项、4,096 个文件、1,024 个 UTF-16 相对路径单元和 128 MiB 文件内容；超过上限或扫描失败只关闭该 package proof，不改变独立 quota gate。
- crates/platform/src/windows_container/mcp_package_tree.ps1 是算法与上限的唯一实现源；broker 用 include_str! 将同一源码嵌入 guest probe，scripts/compute-mcp-package-tree-sha256.ps1 则 dot-source 它，供管理员在将 package 烘焙进固定镜像并填写 v6 catalog 前生成摘要。工具只读 package 目录并向 stdout 输出 lowercase SHA-256。
- 固定启动 probe 对每个 catalog package 重新完整扫描并哈希 guest 文件树；仅当 tree 摘要匹配、entrypoint 在树内且 Node runtime 证明成功时，才产生进程内 package proof。固定 profile 不接受调用方提供的路径、脚本或摘要。
- 为避免 Node 从 package 树外加载未证明的常规模块，probe 拒绝 package 根的 node_modules 父目录，并且只有先匹配固定 node.exe 摘要后才执行 Node 查询 Module.globalPaths；任一 global module directory 已存在时，Node/package proof 保持关闭。Haven 的固定 process environment 不传入 NODE_PATH。
- quota marker 的 package 行升为 HAVEN_MCP_PACKAGE_ATTESTED_V2。缺失、畸形、重复或未知 package 行仍不授予 package proof；quota、Skill runtime 和 Node runtime 的独立状态按既有规则处理。
- 证明覆盖 package 根下普通文件的相对路径与主数据流字节；不把空目录、NTFS alternate data streams 或显式硬编码的树外相对/绝对文件访问宣称为 package 内容证明。镜像仍须由管理员以固定 catalog 和受保护 image layers 管理。

## 替代方案

- 继续只验证 entrypoint：拒绝，依赖和资源变化会改变实际执行内容而不改变 proof。
- 只验证 manifest 文件摘要：拒绝，未经逐文件重算时不能证明 manifest 对应当前 package 文件集合。
- 将文件列表或 package 路径由请求方送入 guest：拒绝，会将执行目标和信任根交给调用方。
- 扫描失败时关闭整个 broker quota gate：拒绝，package identity 与 HCS storage quota 是独立能力。

## 影响与回滚

受保护的管理员 catalog 从 v5 更新为 v6，需重新 provision 每个 tree_sha256；用户 MCP 配置、数据库、broker wire protocol、工具授权和 UI 均不变，无需重置用户数据。未迁移的 v5 catalog 会让 broker 启动失败，符合当前显式管理员部署契约。回滚时恢复 catalog v5 parser/probe 并重新 provision v5 catalog；不得在单一二进制中同时接受两种证明语义。

## 验证

- catalog 测试覆盖 v6 tree digest、畸形/缺失 digest、未知字段、旧 v5 拒绝和 package 数量上限。
- 启动探针测试覆盖完整树扫描、固定路径、排序、文件/字节/路径上限、父级 node_modules 拒绝、Node 摘要校验先于执行及 global module path gate；quota evidence 测试拒绝旧版 package marker。
- compute-mcp-package-tree-sha256.ps1 在临时多文件 package 上与独立 canonical digest 计算一致；改变依赖文件会改变 package tree digest。
- 尚未运行真实 HCS、guest quota 超额写入、镜像部署或目标 Windows build 验收；它们仍是发布 Gate。
