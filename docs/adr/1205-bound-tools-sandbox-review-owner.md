# ADR 1205：为 Tools 增加有界 sandbox review owner

## 状态

`ToolsRuntime` 内的 pending-review owner、预留/保留/查询/丢弃端口和过期回收已实现。前台 Shell 生产调用、proof 绑定的文件晋升及用户逐文件确认仍待接线；在这条调用链完成前，产品 Shell 不创建或保留 AppContainer review。

## 背景

ADR 1203/1204 为单次已退出进程提供 RAII review owner 和可计量的存储上界，但单个 owner 不能限制多次执行同时或累计保留的空间。把 review 放进普通 map 会让活跃子进程与待审数据缺少统一预算，并可能在 registry 锁内递归删除临时目录。

## 决定

- 唯一权威 owner 位于 `haven-tools::ToolRuntime`。App-facing `SandboxOutputReviewPort` 只提供 reserve、manifest snapshot 与 discard，不返回 `AppContainerOutputReview`、目录 capability 或 sandbox 路径。manifest 展示行带有 owner 分配的 `change_index`；晋升调用只能用该索引重新查找 owner-held manifest，不能信任 renderer 发送的源路径。
- key 复用现有规范 `session_id` (`ses-*`) 与源 `step_id` (`step-*`)；同一步只能有一个 active reservation 或 pending review，不增加新的实体 ID 空间。
- 所有运行中 reservation 与 pending review 共用配额：最多 8 项，累计 charge 最多 512 MiB；单次 charge 也不得超过 512 MiB。reservation 的上界必须在启动 sandbox 前提供，retain 时用 ADR 1204 的实际保守 charge 核对；失败或取消时 RAII drop 释放 reservation。容量不足直接拒绝，不驱逐用户仍可审阅的旧结果。
- pending review 保留 15 分钟，owner 在应用 `wire_startup` 后每 30 秒执行一次过期扫描。过期和显式丢弃先从 registry 中移除，再把 RAII owner 移出锁并交给 blocking worker 清理；正常应用退出时 drop `ToolRuntime` 也会释放全部 review。reservation 不设独立期限，其生命周期由执行 attempt 的 RAII owner 管理。
- manifest 只含相对路径的显示字符串、change 类型、字节数和摘要，不含文件内容或绝对路径。相对路径展示可能对非 UTF-8 文件名使用 lossy 文本；选择仍以原 manifest 的 `change_index` 解析，不以显示字符串定位源文件。
- 本切片只建立有界生命周期 owner。review 晋升仍须由 Tools 生成绑定 source path/hash 与 destination 的授权 request，在操作期间持有实时 policy lease，并从当前 `FileWritePolicy` 解析目标 capability；本 ADR 不构成写入授权。

## 替代方案

- 无容量上限的 map 或只限制条数：拒绝，单个 review 的大小差异很大，无法形成磁盘硬上限。
- 容量满时自动淘汰最旧的 review：拒绝，可能在用户确认前删除仍可审阅的数据；新 attempt 应明确失败。
- 在 registry 锁内 drop review：拒绝，TempDir/profile 清理会执行文件系统副作用并延长所有 review 操作的临界区。
- 为 review 生成新的持久 ID：拒绝，review 是进程内生命周期资源，复用规范 session/step 身份足以限定原始调用。

## 影响与迁移

新增 Windows-only Tools runtime capability 和 IPC 之前的只读 DTO，没有数据库、配置或持久事件变化，无需重置。现有 shell 执行仍不调用该 owner；下游接线完成前不会有 review 被产品保留。运行时退出会清理正常 RAII owner，崩溃遗留临时目录的回收仍由后续 scratch cleanup 处理。

## 验证

- Tools Windows 测试通过真实 AppContainer 子进程验证预留转 review、capacity/duplicate key 拒绝、15 分钟生命周期的可注入短 TTL、文件系统 owner 过期清理和显式丢弃。
- 另有纯 owner 测试验证规范 ID、active reservation 计入配额、byte budget、重复 key 和 RAII capacity release。
- 验收运行 `cargo test --locked -p haven-tools`、workspace check/严格 Clippy、格式、ADR 索引和 crate dependency inventory。

## 回滚

移除 Tools review owner、runtime port 和对应测试，再撤回本文、索引及路线图记录。没有持久数据需要迁移或恢复。
