# ADR 1210：回收崩溃遗留的 AppContainer scratch

## 状态

已采纳。

## 背景

AppContainer workspace、只读 baseline 和短期输出 review 已具备 RAII 生命周期，但 Haven 异常退出时析构不会运行。系统 Temp 下可能长期残留私有目录和唯一 AppContainer profile。直接按目录前缀扫描删除不安全：另一个 Haven 实例可能仍在使用对应进程树或 pending review。

## 决定

- 所有 AppContainer workspace 与 baseline 放在 `default_runtime_temp_root()/appcontainer-workspaces` 专用根下；目录操作由打开后持有的 `DirectoryCapability` 完成。
- 每个 attempt 先创建 sibling owner 文件，写入并同步版本化记录，再创建 workspace/baseline 目录或 AppContainer profile。每项资源成功创建后，owner 记录再持久化对应的 created 标志；回收只删除记录标记为本 attempt 创建的对象。记录只包含严格校验的 Haven 临时目录名、`HavenSandbox-file-*` profile 名和三个 created 标志，不包含路径、命令或用户内容。每个 owner 文件由该 attempt 保持独占锁，锁随 AppContainer child、进程树和 pending review owner 的完整生命周期存活。
- 启动新的 workspace 时执行有界机会性回收：最多检查 4096 个 root entry、每次最多回收 8 个 owner；记录 mtime 至少 24 小时、exclusive lock 可获取、记录版本/字段/名称均通过校验后，才删除其 workspace、baseline 和 profile。缺失目录/profile 视为已清理；Microsoft API 明确删除不存在的 profile 会成功。任一步失败则保留 marker，以便下次重试。[API contract](https://learn.microsoft.com/en-us/windows/win32/api/userenv/nf-userenv-deleteappcontainerprofile)
- 目录创建采用 capability 相对的 create-if-absent；回收使用 pinned root 的 capability 删除，不跟随 reparse point。owner 文件缺失、锁仍被持有、mtime 太新、内容无效、命名空间不匹配或无关 entry 一律保留。
- Windows profile 创建与 owner 文件写入不是一个原子事务。若进程恰在 profile 创建成功与其 created 标志持久化之间崩溃，回收会保守保留该 profile 以避免删除未经 owner 记录证明的身份；正常写入失败会立即尝试关闭该间隙并删除新建 profile。
- 该恢复机制仅解决进程崩溃后的资源回收，不是运行期磁盘配额，不能解除 ADR 1209 对 WorkspaceWrite opaque process 的 fail-closed 门槛。

## 替代方案

- 按 Temp 全目录扫描并删除特定前缀：拒绝；无法判断目录是否仍被另一个 Haven 进程持有，也容易扩展删除范围。
- 仅依赖 `TempDir::drop`：拒绝；进程崩溃时析构不会执行。
- 立即删除 owner 不可锁定但较新的 workspace：拒绝；mtime 与运行时长不是活性证明，必须同时满足年龄门限和锁可获取。

## 影响与验收

- `DirectoryCapability` 增加相对 root 的 create-if-absent 文件/目录原语，避免以 ambient path 创建回收标记和 scratch 目录。
- 生产 workspace 在 marker 完整持久化前不创建目录或 profile；正常清理先等待 process tree 归零，再删除目录/profile，最后删除 marker。清理失败时 marker 保留供后续机会性回收。
- Windows 平台单测覆盖 owner record 与创建标记校验、活动锁/新记录/无关 entry 保留、失败 profile 清理后的重试记录、未由记录证明创建的目录保留、严格目录与文件创建、重复项与 parent traversal 拒绝；`appcontainer_probe_tests` 继续走生产 workspace 与 launcher。
- 验收：`cargo test --locked -p haven-platform`（46 项通过）、`cargo clippy --locked -p haven-platform -- -D warnings`。无需数据库或用户配置重置；旧临时目录不按本契约扫描，可能保留到用户清理系统 Temp。

## 回滚

回滚实现后，不再扫描 `appcontainer-workspaces`。回滚不触及 canonical `ExecutionWorkspace`、项目文件或数据库；该目录中的遗留项可由用户删除系统 Temp 中的 Haven 专用目录。
