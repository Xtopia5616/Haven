# ADR 1239：由 broker 持有私有 workspace 暂存区

## 状态

已实施固定暂存根安装、broker capability 打开、单 attempt owner/锁和有界过期回收；尚未接入 named-pipe workspace receiver 或 HCS provider。服务仍在 quota runtime proof 前对合法 `Execute` 返回 `quota_unattested`。按用户要求，本阶段只实现功能，不安排 Windows Container 实机验收。

## 背景

ADR 1238 已实现 capability-rooted workspace 文件传输，但 receiver 需要一个由 broker 独占的 staging capability。暂存目录不能由桌面进程创建后把 host path 交给特权服务，也不能在进程崩溃后无限积累。当前协议对单次输入快照的总字节数设有 4 GiB 上限，可用于限制 broker 同时保留的输入树数量与容量。

## 决定

- 显式安装 broker 时，在固定 `%ProgramFiles%\Haven\ContainerBrokerRuntime` 创建运行目录。受保护 DACL 只授予 SYSTEM、Administrators 与 `HavenContainerBroker` restricted service SID；服务 SID 从固定 SCM service 名解析，不要求安装器本身运行在服务 token 下。路径必须是普通本地目录，拒绝 reparse point。
- 服务启动时用 `DirectoryCapability` 打开并持有该目录。服务启动不做递归回收；新 attempt 分配前才执行 stale cleanup，避免大目录清理延长 SCM 启动等待。
- 同一运行根最多容纳一个 attempt。创建时先以 `create_new` 建立 `owner-file-{uuid32}.lock`，取得排他文件锁并同步，再创建同 ID 的 workspace 目录。marker 文件名中的规范 `file-*` 是 attempt 与目录之间的权威关联；整个运行根由受保护 DACL 限制，调用方不能提供目录名或绝对路径。
- 活跃 `BrokerWorkspaceAttempt` 持有 root/workspace capability 与 owner 锁。正常清理按 capability 删除本 attempt workspace，再删除 marker；清理失败时保留 marker，使后续调用 fail closed。Drop 会尝试同一清理路径。
- 每次创建 attempt 前，只回收 marker 修改时间至少 24 小时且排他锁可获取的 attempt。marker 文件名必须严格匹配 owner 前缀/后缀和 canonical `file-*`；回收只删除其关联 workspace。未知文件、无 owner 目录、年轻 marker、锁仍占用或异常项均保留并阻止新 attempt。
- 扫描最多读取 4096 个 root 项，每轮最多清理 8 个 attempt。单 attempt 约束与 ADR 1238 的文件树总字节限制共同把受管输入快照限制为单请求的 4 GiB transfer ceiling。
- 此 host staging 上限不等于 HCS scratch 的 OS 硬配额，也不证明 guest 无法超额写入。此切片不连接 receiver/provider、不创建 compute system、不开放 `WorkspaceWrite`；ADR 1231 的 quota runtime attestation 仍是生产执行硬门槛。

## 替代方案

- 在桌面进程创建暂存区并把绝对路径发给 broker：拒绝，会让高权限 broker 接受未经自身 capability/owner 管理的路径。
- 允许多个并发 attempt：拒绝，增加 aggregate retained storage 计算与清理竞态；当前串行 broker 无需此能力。
- 对运行根中任意未知项递归清理：拒绝，可能删除不属于已验证 attempt 的数据。未知项保留并令根不可用，直到受控恢复。
- 服务启动时同步回收：拒绝，workspace 可达到 4 GiB，递归删除不应占用 SCM 启动期限；回收在 attempt 分配前按需执行。

## 影响与迁移

新增平台内部 broker scratch owner，并把安装路径与 service startup 接到固定运行根。无数据库、用户配置或 IPC schema 变化，不需要重置。该目录只承载短期输入 staging；服务卸载不自动删除它，避免在未检查锁和 owner 状态时递归删除。

## 验证

- `cargo fmt --package haven-platform -- --check`
- `cargo check --locked -p haven-platform`
- `cargo clippy --locked -p haven-platform -- -D warnings`
- `cargo test --locked -p haven-platform`
- 单测覆盖正常 attempt 清理、活跃锁阻止回收/并行 attempt、过期且 unlocked attempt 回收，以及无 owner 目录保留并阻止新 attempt。
- 未执行提升安装、SCM 服务启动、restricted token DACL 验收、named-pipe 集成、HCS runtime 或 guest 超额写入验收；用户明确要求先实现，不安排实机验收。

## 回滚

移除 broker scratch module，并撤销 broker 安装和 service startup 对该模块的调用即可恢复 ADR 1238 的未连接状态。回滚时不自动删除运行目录；如已有内容，应停止 broker 后依据 owner marker 人工检查并清理，不能递归删除未知项。
