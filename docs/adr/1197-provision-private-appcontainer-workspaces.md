# ADR 1197：创建绑定执行快照的私有 AppContainer 工作区

## 状态

Windows `haven-platform::windows_sandbox::AppContainerWorkspace` 已实现并由真实 AppContainer 探针验证；生产 Shell、Skill 和 MCP launcher 尚未使用它。

## 背景

ADR 1195/1196 确定了 `ExecutionWorkspace` 保持 canonical integrity、AppContainer 写入进入独立 scratch、输入经有界 capability 快照，以及错误输出不得整体晋升。此前真实 AppContainer 验证只在测试中临时改动 fixture 的 DACL/mandatory label，生产代码还没有 workspace owner 来统一持有 package SID、目录句柄和生命周期。

## 决定

- `AppContainerWorkspace::create` 为单次 workspace 创建唯一 AppContainer profile 与系统临时目录；源、目标都通过 `DirectoryCapability`，输入复制遵循 ADR 1196 的条目、深度、文件、总量和取消边界。
- 只修改新建的 sandbox 根：原 DACL 保留并增加该 profile package SID 的继承访问 ACE；根设置 inheritable Low integrity mandatory label，使随后创建的目录和文件具有匹配 label。`ExecutionWorkspace` 不传入 ACL/SACL 修改路径。
- owner 保存 workspace generation、policy revision、快照结果、目录 capability 与 profile SID。该组数值是绑定标签，不单独构成授权；之后的 launcher 必须从当前授权 owner 得到同一 attempt context，并在创建进程前重新核验 generation/revision。
- 初始化失败时 RAII guard 尝试恢复目标根原来的 DACL/label，临时目录与唯一 profile 随后回收。成功 owner 通过 `cleanup()` 明确关闭 capability、删除临时目录和 profile；`Drop` 是错误路径的 best-effort 清理。调用方必须等所有使用该 profile 的子进程退出后再清理 owner。
- 该 owner 准备文件权限和身份，但本身不改变子进程 token，也不创建 OS 进程/网络边界。没有产品 launcher 消费该 API 前，不得声称 Shell、Skill 或 MCP 已隔离，也不得因此在 `WorkspaceWrite` 下开放这些 opaque capability。
- profile 创建时没有附加 optional capability。后续 launcher 需显式使用该 package SID 与空 capability 列表；`Restricted` 网络仍保持 fail closed，不能映射为 `internetClient`。
- 临时目录意外退出后回收、进程 owner join、过滤标准流/handle list 和结果 manifest 晋升尚未实现，继续作为 ADR 1195 的未完成契约。

## 替代方案

- 在 AppContainer 测试或产品代码中降低 canonical 项目根的 mandatory label：拒绝，会改变用户项目和同一用户其他低完整性进程的写入边界。
- 用稳定共享 profile 承载所有并发 sandbox：拒绝，每个 attempt 使用随机 profile SID，避免不同 workspace 共用 package identity。
- 成功后保留 AppContainer profile：拒绝，profile 与 workspace 一同按 attempt 回收，避免无界累积和跨执行持久状态。
- 仅建 `TempDir` 而不设置 package SID DACL 和低完整性 label：拒绝，AppContainer 将无法按预期写入；这仍不是 process sandbox。

## 影响与迁移

新增 Windows 专用 platform API，并将 `tempfile` 用于生产 scratch owner；配置、IPC、数据库与现有进程行为不变，无需重置。当前没有产品消费者；旧行为仍按 ADR 1190 fail closed。

## 验证

- Microsoft 的 AppContainer 指南说明，低完整性进程写入还需对象的 mandatory label 与 package/capability SID DACL 同时允许；Windows ACE 继承规则说明 object/container inherit 会传播到文件和子目录：[AppContainer implementation](https://learn.microsoft.com/en-us/windows/win32/secauthz/implementing-an-appcontainer)、[Mandatory Integrity Control](https://learn.microsoft.com/en-us/windows/win32/secauthz/mandatory-integrity-control)、[ACE inheritance rules](https://learn.microsoft.com/en-us/windows/win32/secauthz/ace-inheritance-rules)。
- Windows integration probe 使用该生产 workspace owner 创建快照并启动 AppContainer helper：容器能写私有副本，不能读写副本外哨兵，且无网络 capability 时不能连接 loopback。
- probe 验证 `workspace_generation`/`policy_revision` 被 owner 捕获、canonical source 未被容器写入、测试结束后 scratch 已删除。
- `cargo test --locked -p haven-platform`、严格 Clippy、locked check、格式检查、ADR 索引与 crate 依赖清单通过后提交。
- 当前验证尚不覆盖生产 Shell/MCP/Skill 的 launcher、子进程 handle 传递、运行中策略变化或崩溃后 stale workspace 回收。

## 回滚

删除 Windows workspace owner、对应平台测试与本文/索引/路线图记录。无持久格式或产品运行时行为需要回滚；若已有临时目录因进程崩溃遗留，清理工作由后续 stale workspace 生命周期切片定义。
