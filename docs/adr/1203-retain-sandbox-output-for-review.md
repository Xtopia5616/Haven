# ADR 1203：由独立 owner 保留 sandbox 输出供后续审阅

## 状态

平台级 `AppContainerOutputReview` 已实现。产品级 pending-review registry、数量/磁盘/时限配额与 UI lifecycle 仍待实施；在这些约束接通前，产品执行路径不得持久保留该对象。

## 背景

ADR 1202 添加的输出晋升仍由 `AppContainerChild` 持有。正常 Shell 工具返回时 child owner 会关闭句柄并删除临时 workspace，因而无法先把 manifest 展示给用户、待用户选择和授权后再执行晋升。直接让 child 持有到用户操作完成会同时占用进程、管道与 Job Object 句柄，也模糊“进程树已归零”和“输出审阅生命周期”的责任。

## 决定

- `AppContainerChild::take_output_review` 仅在 launcher 已确认整个 Job Object 进程树归零后执行有界 manifest 扫描，并把私有输出 workspace 与 manifest 所有权移交给独立 `AppContainerOutputReview`。
- 移交后 child 只保留已退出进程及标准流句柄，不再暴露 workspace 根或 capability；child 可以立即 drop。review owner 持有普通完整性输出目录和私有基线，允许用户界面生命周期在进程关闭后继续审阅。
- `AppContainerOutputReview` 只提供不可变 manifest 视图、ADR 1202 约束下的新文件晋升和显式清理。丢弃 owner 或调用 `cleanup` 会删除输出与 baseline；它不实现序列化、数据库持久化或自动恢复。
- 调用方的 pending-review registry 必须在创建此 owner 前后实施全局容量与过期清理，并在应用退出时 drop 所有 pending owner。未实现有界 owner 前不得把该类型放进无界 map、会话 transcript 或 durable ToolRun。
- Review owner 仍不授权文件写入。实际晋升必须由 Tools 只在 fresh confirmation proof、当前 `FileWritePolicy` capability 与 live `ExecutionPolicyAuthorization` 租约覆盖下触发，并重新校验 generation/revision；这仍是后续切片。

## 替代方案

- 保留整个 `AppContainerChild` 等用户审阅：拒绝，会长期占用进程/管道/Job 句柄，并把进程生命周期与 UI 生命周期耦合。
- child drop 时自动清理输出：拒绝，用户尚无机会查看 manifest 或选择晋升项。
- 把 manifest 和文件内容写进 transcript 或数据库：拒绝，manifest 是进程内授权审阅提示，输出受有界 scratch owner 管理；持久化会引入重启后资源恢复和重复晋升契约。
- 无限期保留各个 review owner：拒绝。平台只提供显式单个 owner，pending 数量、总字节与超时由后续应用级 registry 负责。

## 影响与迁移

新增一个进程内 RAII workspace owner，没有配置、数据库、IPC 或外部 ID 变化，无需重置。已有 child drop 仍自动清理未移交的 sandbox。当前未将 review owner 放入 App、Tools 或 UI 状态，产品 Shell 继续使用现有 launcher。

## 验证

- Windows AppContainer probe 在子进程树存活时拒绝转移；树归零后转移成功，立即 drop child 后 sandbox 输出仍存在；使用 review 晋升后显式 cleanup 会删除 sandbox 和 baseline。
- 完整 `haven-platform` 测试覆盖实际 AppContainer launcher、Job tree drain、输出 manifest、晋升和 owner 生命周期。
- 有界 pending owner、用户逐文件确认、Tools proof/实时策略租约和应用退出清理仍待后续实现，不能以本 ADR 视为产品审阅流程已完成。

## 回滚

移除 `AppContainerOutputReview` 与 child 的 owner 转移入口，再撤回本文、索引及路线图更新。无需恢复持久数据。
