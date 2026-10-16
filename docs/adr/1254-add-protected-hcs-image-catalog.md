# ADR 1254：增加受保护的 HCS 本地镜像目录

## 状态

已实现目录 owner、严格 manifest 校验和 broker 安装/启动接线。管理员仍须单独提供符合许可的本地 Windows 容器镜像；quota runtime attestation 未完成，服务继续 fail closed，不创建 HCS system。

## 背景

HCS 配置已经要求可信本地镜像层，但尚无生产镜像来源。直接从执行请求接受 layer ID、宿主路径或 HCS JSON 会让 broker 的特权边界受不可信调用参数控制。ADR 1220 规定 Haven 不分发基础镜像，镜像与启动配置由受信任服务固定。

## 决定

- 固定镜像根为 `%ProgramFiles%\Haven\ContainerImages`。显式 broker 安装创建该目录并设置受保护 DACL：SYSTEM 与 Administrators 有完全控制权，restricted broker service SID 只有读/执行权限。运行时 scratch 仍按 ADR 1239 授予 broker 写权限。
- 管理员在镜像根下安装 `catalog.json` 和 `layers/`。manifest 是版本 1 JSON，只接受 `version` 与有序 `layers` 字段；每层只有 `id` 和 `directory`。拒绝未知字段、空/超大 manifest（上限 64 KiB）、空层集、超过 64 层、超长/控制字符 ID、重复 ID、重复目录名和不支持的版本。
- `directory` 只能是 `layers/` 下单层 ASCII 名称（字母、数字、`_`、`-`，最多 64 字节）。manifest 必须与 `layers/` 的直接子目录集合完全对应；符号链接、重解析点、特殊项、未列出目录和缺失目录均拒绝。HCS layer 顺序严格沿用 manifest 顺序。
- broker 不提供按请求选择 image/catalog 的字段。服务启动时读取唯一固定 manifest，缺失或无效就不启动；每层由 `DirectoryCapability` 打开并在 catalog 生命周期中保持 pinned。生成的 `WindowsContainerLayer` 和 HCS config 不再提供生产环境下的任意宿主路径构造入口。
- Haven 不下载或包含 Windows 镜像。管理员负责镜像来源、许可、层顺序与更新；更新 catalog 或 layer 目录需要在 broker 停止时进行，再启动时完整重新校验。
- 本切片只准备可信镜像来源，不接入 guest lifecycle 或执行 provider，也不改变 quota runtime gate。`WorkspaceWrite` 继续 fail closed；未做真实 HCS、许可或超额写入验收。

## 替代方案

- 由客户端/执行请求传入 image ID 或路径：拒绝；会扩大特权 broker 的可选操作面。
- Haven 静默下载、附带或自动更新基础镜像：拒绝；镜像许可与宿主适用范围由部署者确认。
- 使用任意层路径后只检查字符串在 Program Files 下：拒绝；路径前缀检查无法固定目录身份，也不能拒绝 reparse substitution。

## 影响与迁移

显式 broker 安装新增受保护的 `ContainerImages` 根目录；管理员需在启动 broker 前安装有序 `catalog.json` 和本地层目录。服务启动会验证 catalog，缺失/损坏时失败。没有数据库、用户配置、IPC schema 或公共 Tools API 变化；quota gate 仍在 workspace staging 和 workspace 内容读取之前拒绝执行，无需重置。

## 验证

- 单测覆盖 manifest 字段/版本拒绝、路径 traversal、重复 ID、缺失/未列出的层目录、manifest 顺序，以及层 capability 随 HCS config 保持存活。
- `cargo fmt --package haven-platform -- --check`、`cargo test --locked -p haven-platform`、`cargo clippy --locked -p haven-platform -- -D warnings` 与 ADR index checker。
- 没有调用 HCS API 或检查目标机许可；真实 quota runtime attestation 与 guest 超额写入验收仍未运行。

## 回滚

移除 catalog module、broker 安装/启动接线与 `ContainerImages` 目录即可回滚本切片。未连接的 HCS provider 不得绕过镜像 catalog 或 quota gate 执行。
