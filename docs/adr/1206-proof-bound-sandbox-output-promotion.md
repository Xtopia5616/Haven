# ADR 1206：以执行 proof 约束 sandbox 输出晋升

## 状态

Tools 内部授权请求、实时策略租约和逐文件晋升路径已实现。Tauri 一次性确认续接、review UI 与前台 Shell 生产接线仍待实施；完成前，产品入口不会保留或晋升 sandbox 输出。

## 背景

ADR 1202 提供了平台级 create-if-absent 文件复制原语，ADR 1205 建立了按 session/step 管理、具备容量与 TTL 上限的 pending-review owner。两者之间仍缺一条 Tools 授权路径：不能让 renderer 提交 sandbox 源路径或直接持有 `AppContainerOutputReview`，目标也必须重新经过当前 Files workspace 与 `allowed_paths` 策略。

## 决定

- 晋升是独立的 `files.promote_sandbox_output` 授权 capability，效果为 `WorkspaceWrite`、网络访问为 `None`，每次调用都要求新的用户确认。请求携带规范 session/step、owner manifest 的 `change_index` 和唯一目标路径；opaque proof 同时绑定完整输入和 source-entry target binding。
- Tools 只接受 `AuthorizationPort` 为该确切请求签发的 execution proof。执行时重新验证 proof，并从该 proof 捕获的 `FileWritePolicy` 解析目标 `DirectoryCapability`；不接受 renderer 提交的源路径、绝对 sandbox 路径或 capability。
- 目标能力捕获后，Tools 获取 proof 派生的持有式实时 policy read permit，并将 permit 移交给执行有界文件复制的 blocking worker。外层 Tauri task 被取消也不能提前释放租约；workspace generation 和 policy revision 必须与 review 创建时记录的 stamp 一致，策略或 workspace 更新要么先完成并使 proof 失效，要么等待文件副作用真正结束。
- owner 按 `change_index` 读取不可变 manifest，只允许 `Created` 普通文件；复制过程重新校验源文件大小与 SHA-256，通过已解析的 `DirectoryCapability` 原子创建缺失目标，绝不覆盖已有文件，也不创建父目录。修改、删除、目录、类型变化和特殊文件保持不可晋升。
- 一个 review 同时只允许一个晋升操作；晋升期间不能丢弃或过期回收该 review。单文件晋升成功后 review 保留到原 TTL，用户可分别确认并晋升其它文件；每次尝试都须创建新 proof。失败或取消不消耗 review，平台原语清理由目标侧未提交的暂存内容。
- Tauri 续接必须把批准收敛为 Once；该确认不可授予 session 或永久权限。confirmation interaction 可以由 AppCommand owner 管理，但私有授权请求必须保留 review 所属 session，以便执行时照常检查该 session 的 deny 规则。
- 当前切片只实现 Tools 侧路径，不增加 IPC、配置、持久事件或数据库状态。review UI 与 Shell 生产接线完成前不得把 sandbox review 暴露为可操作产品功能。

## 替代方案

- renderer 发送源路径或直接写文件：拒绝，renderer 不是 sandbox 输出和文件系统授权的 owner。
- 将整个 sandbox 目录复制回 ExecutionWorkspace：拒绝，会绕过逐项审阅、路径授权和 create-if-absent 语义。
- 复用 `files.write` 的宽泛授权：拒绝，文件晋升需要可单独拒绝/审计的 capability 与每次新确认。
- 审阅时即预先绑定目标 capability：拒绝，用户选择目标后必须用当前 live policy 重新解析，不能持有过期目录授权。
- 晋升 review 内的修改或删除：暂缓；需要覆盖/删除专用的竞态安全原语与新的确认契约。

## 影响与迁移

新增 Tools 内部 proof 请求构造与执行方法、每个 review 的晋升串行状态，以及成功结果中的字节数与 SHA-256。当前没有 IPC 或持久数据变化，也没有把 Shell 切换到 AppContainer，无需数据库重置或兼容迁移。

## 验证

- Tools 测试验证 proof 请求绑定 session、source step、manifest index、目标路径和强制确认策略；模拟取消等待 task，验证阻塞文件 worker 持有的 live policy permit 仍会串行化后续策略更新。
- Windows AppContainer probe 验证 owner 通过 manifest index 晋升新文件、workspace generation 不匹配时拒绝且不创建目标。
- 本切片验收运行 `cargo check --locked -p haven-tools`、定向 sandbox review 与 request-builder 测试、严格 Clippy、格式及 ADR 索引。Tauri confirmation、Files policy 更新竞态和真实 Shell 端到端验证属于后续接线切片。

## 回滚

移除 Tools proof 请求构造、晋升执行方法和 owner 串行状态，再撤回本文、ADR 索引与路线图更新。没有持久状态需要迁移或恢复。
