# ADR 1224：不向容器 provider 暴露 workspace 根路径

## 状态

已实施于平台执行端口；没有 HCS provider 或工具接线。

## 背景

ADR 1223 的端口直接接收 `DirectoryCapability`，其公开 `root()` 可返回宿主绝对路径。虽然契约禁止把路径放入 broker IPC，但 provider 仍能读取并误传执行 workspace 的真实路径，削弱了“文件流 staging 而不是 host path 映射”的边界。

## 决定

- `WindowsContainerExecutionPort` 改为接收 `WindowsContainerWorkspaceInput`，它在内部持有已授权的目录 capability，但不提供 capability/root getter。
- provider 只能调用 `copy_snapshot_to`，将源树按既有 `DirectoryTreeCopyLimits` 复制到调用方提供的私有 staging capability。调用方在任何失败或取消后都必须丢弃部分 staging；不允许将完整项目根映射为容器可写路径。
- source 和 private staging 的绝对路径都不进入 broker IPC。后续 broker 协议必须显式遍历并发送规范相对路径与有界文件字节流，不能序列化本地 capability 或 staging 根路径。
- 该包装只限制 provider 能观察的 API 表面；它不替代 Tools 授权、实时 policy permit、HCS storage quota、输出 review 或实际容器隔离验收。

## 替代方案

- 只靠注释要求 provider 不读 `DirectoryCapability::root()`：拒绝；安全边界应由窄类型表达，不依赖实现者自律。
- 为执行端口复制整个 `DirectoryCapability` 或新增 `root()` 转发 getter：拒绝；会继续让宿主路径可见。
- 让 provider 直接打开任意源相对路径：拒绝；会绕过统一的有界快照和 reparse 拒绝逻辑。

## 影响与迁移

更新平台执行端口参数并新增有界快照单元测试；无生产 consumer、IPC、配置、持久化或授权行为变化，不需要重置。后续 broker 接入需要从 staging capability 做显式文件流编码。

## 验证

- `cargo fmt --all -- --check`
- `cargo test --locked -p haven-platform`
- `cargo clippy --locked -p haven-platform -- -D warnings`
- 测试验证有界源树复制仍工作；类型不暴露底层 capability getter。
- 该 API 收敛不构成 HCS quota proof，仍须在受支持 Windows 宿主上完成实际超额写入验收。

## 回滚

无需恢复原始 capability 参数；若移除该端口，删除新类型和调用者即可。不能以回滚为由把源码根路径传给 broker。
