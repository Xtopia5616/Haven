# ADR 0470：协调生成媒体写入与 GC

## 状态

已完成（2026-10-05）。

## 背景

ADR 0469 收口了 App 上传和媒体清理，但刻意没有处理 Tools producer 与 App generated-media GC 的跨 crate 竞态。图片生成、录音、截图和剪贴板 producer 都先创建文件，再向 `ManagedAssetRegistry` 登记 session lease 或 detached TTL。App cleaner 可能在文件创建后、登记前读取 lease/TTL 快照并删除该文件；登记晚于快照也不能挽救已进入 unlink 阶段的文件。

启动/每日 GC 与活跃 session 的 Tools 执行可并发；删除历史后也会立即触发 GC。现有 `UPLOAD_WRITE_LOCK` 只串行化 App 上传路径，Tools producer 不持有它。风险不是模块大小，而是文件生命周期在写入与 lease handoff 之间缺少共享同步边界。

## 决定

1. 由现有 `ManagedAssetRegistry` clone-shared 状态持有一个生成媒体生命周期读写 gate；不新增资产映射、reservation 集合或 durable state。
2. 每个 generated-media producer 在创建目标文件前取得读 permit，并持有到完整文件写入和 `register_generated_asset` 成功。登记失败、写入失败或取消时按现有路径清理部分文件；RAII permit 在所有返回、错误和任务取消路径释放。若文件 I/O 已移入 `spawn_blocking`，permit 必须随阻塞闭包一起持有，避免 async caller 被取消后后台写入仍在 gate 外继续。
3. App cleaner 在读取 generated-media lease/TTL 快照前取得同一 gate 的独占 permit，并持有到本轮 generated-media 候选扫描和 unlink 完成。generated-media 阶段结束后即释放；uploads 扫描继续由 App `UPLOAD_WRITE_LOCK` 串行化，不额外阻塞 Tools producer。
4. 锁顺序固定为 App `UPLOAD_WRITE_LOCK` → registry generated-media 独占 gate；Tools producer 只取得 registry 读 permit，不反向获取 App 锁。Tokio `RwLock` 的等待写者优先语义避免 cleaner 等待期间新 producer 持续越过 GC。
5. registry 的既有 lease、pending lease、session 引用和 detached TTL 仍决定文件能否回收；gate 只保证“文件写完并完成登记”与“快照到删除”互斥，不替代任何资产生命周期状态或清理策略。
6. 覆盖图片生成、录音、截图、剪贴板图片和逐文件剪贴板复制。媒体生成的远程请求、录音采集等待、剪贴板读取等不落盘阶段不持有 gate；剪贴板多个文件逐文件获取 permit，避免整个批次长期阻塞 GC。
7. 剪贴板源路径校验与元数据读取放到 blocking pool；复制以 64 KiB 分块检查取消，并在打开源文件后再次执行 64 MiB 单文件上限检查。最多 32 个文件的既有批次上限不变。

## 不变量与验收

- cleaner 先取得独占 permit 时，producer 在 permit 获得前不得创建目标文件；cleaner 完成本轮 unlink 后 producer 才可继续写和登记。
- producer 先取得读 permit 时，cleaner 必须等到完整文件写入且 lease/TTL 登记结束，再读取保护快照；已登记的 session lease 或 detached TTL 应保护该文件。
- producer 的失败、超时或取消不泄漏 permit；文件写失败不留下可被误登记的部分资产。
- 锁不跨 provider 请求、录音时长等待、模型处理或 uploads 扫描持有；GC 的 generated-media gate 必须覆盖 snapshot、遍历和 unlink 的整个窗口。
- 使用 channel/barrier 控制的确定性并发回归测试验证 producer-first 与 cleaner-first 两种顺序，不以 `sleep` 猜竞态时序。
- durable reference 查询失败仍 fail closed；已有双根独立尝试、symlink/reparse 拒绝、lease/TTL、上传 quota 与 staging 语义保持。

本切片不改变 asset ID、数据库/config schema、Tauri IPC、清理频率、保留期或用户数据格式，无需重置数据库或配置。

## 替代方案

- 将 App `UPLOAD_WRITE_LOCK` 暴露给 Tools：拒绝。它是 App 上传与目录扫描的内部锁，跨 crate 暴露会倒置依赖并把 App 实现细节变成 Tools 契约。
- 在 registry 新增 reservation set：拒绝。仅有 reservation 快照仍可能在快照后、unlink 前漏掉新 reservation；正确实现还需要在快照与删除之间继续协调，等同增加第二套资产生命周期状态。
- 仅在文件写完后缩短登记窗口：拒绝。GC 可在文件落地后立刻删除，时间窗口变小不构成正确性保证。
- 把文件扫描、持久引用或清理策略整体搬到 Tools：拒绝。App 仍拥有 durable `SessionStore` 查询和根目录清理策略，registry 只提供跨边界同步端口。

## 影响、验证与回滚

改动限于 `haven-tools` 的 registry 与四类 producer、App managed-media cleaner、回归测试及架构文档。没有数据库/config/IPC 变化，无需重置。

Windows 验收通过：`cargo fmt --all -- --check`、`cargo check --locked -p haven-tools -p haven-app-binary`、`cargo test --workspace --locked` 与 `cargo clippy --workspace --locked -- -D warnings`。另有 `cargo test --locked -p haven-tools asset_registry::tests`（9 passed）及 `cargo test --locked -p haven-app-binary commands::managed_media::tests`（26 passed）。测试使用临时目录、oneshot 与 blocking closure channel 覆盖 producer-first、GC-first，以及两侧 async caller 被取消后阻塞文件操作仍持有 gate；无真实用户目录、剪贴板或屏幕依赖。

一个后续证据项暂不并入本切片：`FilesTool` 的 rich-path handoff 会给已存在的任意绝对路径登记 asset；需单独确认它指向 generated-media 根目录时，是否存在 GC 与读取/登记的用户可见竞争。该路径不创建新生成文件，当前 gate 不扩大到所有文件导入。

回滚时可移除 registry gate 和 producer permit 参数，并恢复当前先写后登记实现；无持久数据迁移。回滚会重新引入 ADR 0469 已记录的并发窗口，应视为放弃该正确性修复。
