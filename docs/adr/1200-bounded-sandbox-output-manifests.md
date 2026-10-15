# ADR 1200：为 sandbox 输出建立私有基线与有界变更清单

## 状态

已实现为 `DirectoryCapability::output_manifest` 与 `AppContainerWorkspace` 私有基线；逐路径授权晋升及产品 Shell 接线仍待实施。

## 背景

AppContainer 进程只写入独立的低完整性 sandbox 副本。要把有意义的结果带回项目，后续不能把整棵 scratch 目录复制回 `ExecutionWorkspace`；必须先识别变更，再按路径重新授权。此前 workspace owner 只有初始复制摘要，没有可靠的输入对照，也没有拒绝重解析点和资源超限的输出扫描接口。

## 决定

- `AppContainerWorkspace` 在创建低完整性工作区后，把同一份初始 sandbox 快照复制到单独的普通完整性临时目录作为输入基线。基线目录不授予 AppContainer package SID，也不继承 sandbox 的 Low integrity label；workspace owner 持有它直至进程树结束并清理。
- `DirectoryCapability::output_manifest` 通过两个已打开的目录 capability 比较当前输出与基线。清单按相对路径列出创建、修改、删除、类型变化及目录变化，并记录文件大小和 SHA-256 前后摘要；它不包含文件内容。摘要只用于内容比对和后续一致性检查，不构成授权。
- 扫描显式使用有界 `DirectoryTreeCopyLimits`。清单最多 4,096 项、单项变化不超过 64 MiB、变化总量不超过 256 MiB、路径累计字节不超过 4 MiB；超限时整份清单失败，不返回部分结果。取消同样返回错误。
- 两个根必须不重叠。遍历拒绝 reparse point、符号链接、特殊文件、无法表示为 Unicode 的相对路径和大小写不敏感的路径冲突；文件在扫描期间改变大小或可观测修改时间时也失败关闭。
- manifest 只能通过 `AppContainerChild` 获取，且须由 launcher 确认根进程退出并且 Job Object 进程树归零；运行中的进程不能与扫描竞争改写清单。此切片不自动写回 canonical workspace、不发送给模型或 renderer，也不增加交互或持久存储。下一步须建立用户可审阅的逐路径提案，并在应用时重新检查当前授权、策略 revision、workspace generation、可写根和工具 `allowed_paths`；在此之前产品 Shell 继续使用原执行路径，WorkspaceWrite 对 opaque Shell 继续 fail closed。

## 替代方案

- 进程退出时整体复制 sandbox：拒绝，会越过 `writable_roots`、工具 `allowed_paths` 和用户确认边界。
- 用 sandbox 路径上的文件时间戳或大小推断内容变化：拒绝，同尺寸覆写可能不变时间戳，无法确认字节是否改变。
- 将文件内容直接放进 manifest：拒绝，会造成未受控的内存、IPC 与模型上下文放大；清单只提供元数据和摘要。
- 把基线放在 AppContainer 可访问的 sandbox 根中：拒绝，进程可覆写基线并抹去其自身变更证据。

## 影响与迁移

平台层增加 SHA-256 输出扫描及第二个临时目录 owner；现有 Shell 行为、配置、IPC、数据库和持久身份不变，无需重置。manifest 是进程内快照，不作为可复用授权证明或 durable artifact。文件职责复核：`filesystem.rs` 在本切片前约 920 行，本次只加 module 声明与 facade re-export；目录 capability 的既有句柄实现暂留原模块，新扫描实现拆到独立 `tree_diff.rs`，后续若继续扩充平台文件能力，应把底层 capability 实现拆到独立实现模块并让 `filesystem.rs` 收敛为 facade。

## 验证

- 平台单测验证增删改与目录变化、摘要差异、路径/根重叠拒绝、扫描取消以及变化数量和字节上限。
- Windows AppContainer probe 额外证明进程可以写自己的副本、不能读私有输入基线，并且进程树退出后 owner 能生成含新文件的 manifest。
- `cargo test --locked -p haven-platform`、严格 Clippy、locked check、格式检查、ADR 索引和 crate 依赖检查作为本切片门禁。
- canonical workspace 中的逐路径确认、竞态安全的晋升、UI 审阅生命周期、scratch 崩溃回收和生产 Shell launcher 仍待后续切片验收。

## 回滚

移除 `DirectoryTreeDiffLimits`、输出 manifest 与 workspace 私有基线，再还原平台依赖和本文/索引/路线图记录。没有持久数据需要恢复。
