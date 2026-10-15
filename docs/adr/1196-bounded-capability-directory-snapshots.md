# ADR 1196：增加有界目录 capability 快照复制原语

## 状态

已采纳并实现为 `haven-platform` 的通用 capability API；生产 `SandboxWorkspace` 尚未接入。

## 背景

ADR 1195 将 canonical `ExecutionWorkspace` 与低完整性 `SandboxWorkspace` 分开，并要求 sandbox 输入是有界、固定 workspace generation 的文件快照。直接降低项目根的 integrity label 或退出后整体复制回项目根都会越过既有授权和目录 capability 晋升边界。

`DirectoryCapability` 已提供句柄锚定的读取和原子文件替换，但还没有对目录树执行统一条目、深度、单文件、总字节和取消限制的能力。让上层各自递归遍历会重复实现链接拒绝与资源预算。

## 决定

- `DirectoryCapability::copy_tree_to` 将一个 capability 树复制到空的目标 capability；源与目标不能相同或路径重叠，目标在操作期间由调用方独占。
- 输入路径及目录枚举通过已打开的 capability handle；拒绝 symlink/reparse point、非普通文件/目录、非单路径组件和目标中的已有内容。目标目录结构在目标 capability 下创建，普通文件经原子文件替换写入。
- 调用方必须显式提供 limits；构造器将其限制在 100,000 个条目、64 层目录、单文件 512 MiB、总数据 4 GiB 的硬上限内。复制以 64 KiB 缓冲区流式读取，按实际读取字节再次检查预算，并提供遍历和文件分块取消检查。
- 复制是有界 best-effort snapshot。若需要点时一致性，上层必须在操作期间阻止源修改；同一大小的并发内容改写不能由该 API 证明为一致快照。
- 取消或错误可能留下不完整目标；调用方必须回收并整体丢弃该目标，不得把目录树整体晋升到 `ExecutionWorkspace`。晋升仍须回到现有授权 proof、当前策略/workspace generation 重检和逐路径 `DirectoryCapability` 写入口。
- 本 API 不设置 ACL、mandatory integrity label、AppContainer capability、process/job 属性或 UI/安全配置，也不代表生产 Shell、Skill 或 MCP launcher 已 sandbox 化。`SandboxWorkspace` 创建、生命周期、OS 授权与结果 manifest 晋升继续由 ADR 1195 约束。

## 替代方案

- 让各子系统自行递归复制：拒绝，会重复实现并可能分叉链接拒绝、句柄锚定与容量边界。
- 复制前完整读取到内存：拒绝，会让最大文件和总量限制直接变成瞬时内存占用。
- 通过 hard link 或 reparse point 复用输入：拒绝，会共享源文件对象或把遍历跳转到不受快照根约束的目标。
- 把复制结果目录整体重命名回用户项目：拒绝，会绕过授权和逐路径确认。

## 影响与迁移

此 ADR 新增 `haven-platform` API 和单元测试，不改变现有配置、IPC、数据库或当前 workspace 行为，无需兼容迁移。目标 capability 必须由调用方独占，失败后应丢弃；本切片尚无生产消费者。

## 验证

- `cargo test --locked -p haven-platform`：27 项通过，其中包括 Windows AppContainer 文件/网络边界探针。
- `cargo clippy --locked -p haven-platform --all-targets -- -D warnings`、`cargo check --locked -p haven-platform`、`cargo fmt --all -- --check` 通过。
- 测试覆盖成功复制、目标非空/根重叠、条目/深度/文件/总量限制、取消 staging 清理、reparse point 拒绝和 hard-limit 参数校验。
- ADR 索引和 crate 依赖清单检查通过。跨源并发修改的一致性与生产 sandbox 生命周期仍未由该 API 覆盖。

## 回滚

删除该 API、测试、本文及 ADR 索引/路线图中的状态说明。它没有持久格式或运行时接线，不需数据回滚。
