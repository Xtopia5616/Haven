# ADR 1266：在容器镜像目录中固定 Skill Python runtime

## 状态

已采纳并实现 catalog v2 校验、启动期 guest binary probe 和独立 runtime gate 状态。该 gate 只证明 pinned profile 的解释器版本/digest；Skill guest 执行与真实 HCS 验收仍未接入。

## 背景

ADR 1265 定义了固定的 `haven-python-skill-v1` profile，但 pinned image catalog 只列出 HCS layers。执行请求不能选择可执行文件，Haven 也不分发镜像；需要一个受保护、按服务启动固定的方式记录管理员部署镜像内 Python 的精确版本与 binary identity。

## 决定

- catalog schema 升至版本 2。`skill_python_runtime` 是可选字段；若存在，必须包含且只包含 `profile`、`version` 和 `executable_sha256`。旧版 catalog v1 拒绝打开，不保留兼容读取。
- 唯一支持 profile 为 `haven-python-skill-v1`。版本使用规范的 `3.<minor>.<patch>` 数字格式；digest 必须是 64 个小写十六进制字符。
- 可执行文件路径固定在 broker 代码中为 `C:\Haven\Python\python.exe`，catalog 不得提供路径、参数、环境变量或其它可执行入口。后续启动 probe 必须通过该路径运行 `--version`，并在 guest 内对 binary 计算 SHA-256，与 pinned catalog 的精确版本/digest 比较。
- quota 启动 probe 同时检查可选 Python profile：仅用固定路径执行 `--version` 并在 guest 内计算 SHA-256。runtime 结果写入受限 marker，并与 quota evidence 分别校验；Python 不存在或不匹配不会使通过验证的 Shell quota gate 关闭。
- process-local `QuotaRuntimeAttestation` 同时保存 quota 证明、profile 是否配置及 runtime probe 结果；认证 status 协议 v6 分别报告 Shell quota 与 Skill Python runtime。profile 缺失报告 `not_configured`；probe 不成功或无法证明时报告 `unattested`。在后续 Skill executor 接入前，此状态不会授权 Skill 调用。
- descriptor 缺失或 probe 失败时，Skill runtime gate 必须保持关闭。descriptor 本身只是管理员声明；未取得与 pinned catalog 绑定的 process-local probe 证明前不能用于 Skill 执行。
- catalog v2 与 HCS layer 仍由受保护 Program Files 根目录固定；descriptor 不从 named pipe 接收，也不接受 renderer 配置。

## 替代方案

- 从执行请求接收 Python 路径或版本：拒绝，客户端可以将 broker 指向宿主可执行文件。
- 只检查路径存在或运行 `python --version` 而不校验 digest：拒绝，路径可替换为另一个管理员未固定的 binary。
- 缺少 Skill Python 时阻断 Shell quota gate：拒绝，独立 Skill 能力不应改变 Shell 可用性。

## 影响与迁移

显式 Windows broker 部署需把 `catalog.json` 从 v1 更新为 v2；旧 catalog 会在服务启动时因版本不匹配被拒绝。Haven 不改写管理员的 catalog 或镜像。该部署文件不是 Haven 持久配置、数据库或用户数据，无需数据库/用户数据重置。回滚时恢复 catalog v1 读取并移除 runtime descriptor 类型；当前没有 Skill runtime 可依赖此声明。

## 验证

- 单测覆盖 v2 解析、profile/版本/digest 格式、未知字段、v1/v3 拒绝、layer pin、guest 探针固定路径和精确版本/digest、quota 与 Python runtime gate 的隔离及 status protocol v6。
- `cargo fmt --all -- --check`、`cargo test --locked -p haven-platform`、`cargo clippy --locked -p haven-platform -- -D warnings`、ADR 索引与 crate 依赖检查按本切片执行。
- 本切片没有启动真实 HCS 或运行 guest Python；自动测试只验证 PowerShell 探针生成、证据解析和 gate 组合。该证明尚未作为 Skill guest 执行授权。
