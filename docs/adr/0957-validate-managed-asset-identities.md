# 0957：校验 ManagedAssetRegistry 的实体身份

## 状态

已接受并实现（2026-10-11）。

## 背景

`ManagedAssetRegistry` 是 Tools 内部 managed asset 的权威 owner；App 只通过 `ManagedAssetLifecyclePort` 请求注册和 session lease。生产 ingress/媒体 producer 负责使用 `new_id("asset")` 生成资产身份，会话身份由 session owner 生成 `ses-*`。但 registry 的注册、解析和 lease 操作此前只拒绝空字符串，因此格式错误的身份也能进入进程内资产 map 或 lease 状态。

## 决定

- 在 registry 的资产注册、解析、存在性查询、pending/session lease 绑定、转移与释放边界统一调用 `haven_common::types::is_canonical_id`，资产使用 `asset` 前缀，会话使用 `ses` 前缀。
- 拒绝非规范身份时返回现有失败值（`false`、`None` 或 `0`），并在写入前校验，保证失败不会添加资产、pending lease 或 session lease，也不会错误释放已有 lease。
- 单元测试也经 `new_id(prefix)` 生成规范 ID；测试覆盖非法资产与会话 ID 的拒绝及其无状态副作用。

## 替代方案

- 保留非空检查：会继续允许错误前缀、长度、大小写或非 hex 身份进入 owner 状态，违反统一实体 ID 契约。
- 在各 consumer 重复校验：会分散权威规则，也无法保护 registry 的其他调用入口，因此校验放在资产 owner 边界。

## 影响

生产身份生成和有效资产生命周期不变；registry 不再接纳不符合统一实体 ID 格式的输入。改动只影响进程内校验，不改变 IPC、SQLite、文件布局或持久化数据，因此无需重置。

## 验证

- `cargo fmt --all -- --check`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `pwsh -NoProfile -File scripts/check-adr-index.ps1`
- `git diff --cached --check`

## 回滚

若未来资产身份改为不同的正式格式，先在 Common 更新唯一格式校验与生成契约，再同步 registry 所有边界；不要恢复各入口的非空字符串检查或局部格式实现。
