# ADR 1204：为 sandbox review 提供保守存储计量

## 状态

已实现平台级 review 存储上界。pending-review owner 的并发预留、总配额和过期清理仍待实施；在其接通前，产品执行路径不得保留 `AppContainerOutputReview`。

## 背景

ADR 1203 将已退出进程的 workspace 与 manifest 交给独立 RAII owner。后续应用层要同时约束运行中的 sandbox 与待审 review，必须在接受 review 前知道其存储成本。只按 manifest 的变化字节计量会漏掉初始工作区副本和不可读 baseline；按文件数计量也无法限制磁盘占用。

## 决定

- `AppContainerOutputReview::retained_storage_upper_bound_bytes` 以 `2 × 初始快照文件字节数 + manifest 中变化后文件的字节数` 作为保守上界。两份初始快照分别对应 sandbox 输出目录中的输入副本和不可读 baseline；变化后的文件字节再计一次，故意不扣除被修改文件的旧尺寸。
- 目录项不增加文件内容字节数；manifest 与快照扫描分别受既有 entry、file、total-byte 限制约束。该值是配额 charge，不声称等于 NTFS 实际分配空间，也不包含目录元数据、profile 或固定 launcher 开销。
- 调用方在保留 review 前应将该 charge 计入 pending 与 active reservation 的合计预算；超限时丢弃新 review 并清理其 workspace，不能静默挤掉其它仍可审阅的输出。
- 此方法不做配额、不持久化状态，也不改变 sandbox 或晋升授权语义。全局 owner 仍由 Tools 应用执行所有者实现，并负责在 mutex 外 drop/清理过期对象。

## 替代方案

- 只计算 `manifest.changed_bytes`：拒绝，未包含初始输出目录和 baseline。
- 只按 review 数量限制：拒绝，单个快照可消耗不成比例的磁盘空间。
- 声称精确测量 NTFS 占用：拒绝，文件逻辑长度不能表示压缩、簇取整、profile 和目录元数据等实际分配成本。

## 影响与迁移

为 Windows-only 平台 review owner 增加只读计量方法，无持久数据、配置、IPC 或外部 ID 变化，无需重置。产品层仍未创建或保留 pending review。

## 验证

- Windows AppContainer probe 对真实复制出的可执行文件及其运行后 manifest 验证 charge 公式。
- 运行 `cargo test --locked -p haven-platform`、workspace 严格 Clippy、格式、ADR 索引和 crate dependency inventory。

## 回滚

移除上界方法、对应 probe assertion、本文与索引/路线图记录。该值尚未写入持久状态，无需恢复数据。
