# ADR 1222：将 sandbox 输出 review 抽象为后端中立 owner

## 状态

已实施并由现有 AppContainer 路径验证。Windows Container/HCS 尚未接入该 owner，也没有改变 `WorkspaceWrite` 的 fail-closed 行为。

## 背景

Tools 的 pending-review owner 直接持有平台类型 `AppContainerOutputReview`。ADR 1220 选择 HCS 后，broker 返回的 workspace 内容不能构造该 AppContainer 专属类型，导致现有 manifest、存储 charge、授权晋升和过期清理流程无法复用。

## 决定

- `haven-platform` 提供统一的 `SandboxOutputReview`，承载不可变 `DirectoryTreeOutputManifest`、保守存储 charge、带 workspace/policy stamp 的 Created 文件晋升和显式清理。
- 输出 workspace 的生命周期/晋升 trait 与 `SandboxOutputReview` 的构造器只在 `haven-platform` crate 内可见。Tools 只接触中立 review 类型，不取得 AppContainer profile、HCS compute-system id、宿主路径或具体 provider owner。
- provider 只有在执行进程树已归零并生成有界 manifest 后才能创建 review。Tools 晋升继续要求当前 authorization proof、实时 policy permit、review owner manifest index 和已授权目标 `DirectoryCapability`；平台中立化不增加权限。
- 当前 `AppContainerWorkspace` 是唯一实现；未来 HCS provider 必须先把受限输出流重建到本地私有 review scratch，再创建相同 review 对象。HCS service 不提供宿主绝对路径，也不通过可写 host mapping 交付文件。
- 当前行为、capacity/TTL 预算和 IPC 不变。此切片不连接 broker、不启用 HCS、不放开 opaque capability。

## 替代方案

- 在 Tools 中为 AppContainer/HCS 各保留一套 review owner：拒绝；会复制同一套容量、过期、授权与逐文件晋升规则并产生策略漂移。
- 让 HCS 伪造 AppContainer profile/workspace 以复用旧类型：拒绝；会把一种 OS 后端身份错误地当作通用文件生命周期 owner。
- 把 manifest 的宿主绝对路径交给 Tools/renderer：拒绝；review API 只用稳定 index 选择变更，真实路径仍由平台与 Tools owner 持有。

## 影响与迁移

无数据库、配置或 IPC schema 变化，不需要重置。Rust API 从 `appcontainer_process::AppContainerOutputReview` 改为平台根导出的 `SandboxOutputReview`；仓库内 Tools consumer 已同步迁移。该项目允许删除此类内部兼容层。

## 验证

- `cargo fmt --all -- --check`
- `cargo test --workspace --locked`
- workspace 严格 Clippy
- Windows AppContainer probe、Tools review 预留/过期/丢弃、策略 stamp、manifest-index 晋升与 proof-bound `DirectoryCapability` 测试。
- 当前测试证明 AppContainer 适配仍工作；HCS owner 与运行期 quota 仍待后续 broker/Windows integration slice。

## 回滚

恢复 `AppContainerOutputReview` 并让 Tools owner 重新持有该具体类型即可；若后续 HCS 尚未接入，没有运行时状态迁移。HCS 接线前不得通过回滚切换为无配额的宿主或普通 AppContainer temp 执行。
