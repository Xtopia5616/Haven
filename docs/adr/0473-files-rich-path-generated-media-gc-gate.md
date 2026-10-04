# ADR 0473：协调 Files rich-path 登记与生成媒体 GC

## 状态

已采纳、实施并验收（2026-10-05）。

## 背景

`FilesTool` 的 `read`/`summary` 可接受绝对路径。对图片、音频、视频和文档，`register_rich_path_asset` 会 canonicalize、读取 metadata，再将路径注册为 managed asset；有 session 时登记 session lease，没有 session 时登记带 TTL 的临时资产。它可以指向 generated-media 根目录中符合 cleaner 命名规则的文件。

ADR 0470 已使 Tools producer 的写入/登记与 App generated-media cleaner 的快照/unlink 互斥，但 rich-path handoff 未取得该 gate。GC 先快照 lease/TTL 后，Files 仍可登记新 lease；cleaner 继续按旧快照 unlink，之后媒体读取会失败，registry 还可能暂时保留失效 lease。绝对路径入口和文件命名规则见 `file_paths.rs`、`managed_media.rs`；GC 不会扫描生成媒体根的子目录。

## 决定

1. 复用 `ManagedAssetRegistry` 的 generated-media 读写 gate，不新增 reservation、资产映射或 App→Tools 依赖。
2. `FilesTool` 先 canonicalize 用户路径。若 canonical 文件是 `default_generated_media_dir()` 的直接子项，则在 metadata、managed-root/reparse-point revalidation、lease/TTL 登记期间取得 registry shared permit；其他路径不占用该 gate。路径通过 junction/symlink alias 指向根内文件时，canonicalize 后仍命中同一 gate。
3. permit 在 `register_path_asset` 完成后释放，不跨越 `MediaTool`、摘要模型调用或普通文件处理。若 cleaner 已持 exclusive permit，它先按快照完成 unlink；handoff 随后 stat/revalidate 失败，不登记缺失资产。若 handoff 先登记，cleaner 后取快照便能看到 session lease 或 transient TTL 并保留文件。
4. tool 的 cancellation token 可中止等待中的 handoff future；future 被丢弃时 RAII permit 随之释放，不留 lease。
5. App cleaner 仍只负责其配置的 generated-media 根目录；gate 只协调其直接文件项，不替代 lease/TTL/durable-ref 保留策略。

## 不变量与验收

- 当 canonical path 是 generated-media 根目录的直接子项时，metadata、registry revalidation 与登记不能穿过 cleaner 的 exclusive snapshot-to-unlink 窗口。
- cleaner-first 的结果是删除先完成、handoff 失败且 registry 无该文件的 lease；handoff-first 的结果是登记先完成、cleaner 快照看见保护租约并保留文件。
- 根目录外的 rich path 不取得 generated-media gate；不存在 permit 覆盖 provider/model/media processing 的情况，并由回归确认 cleaner 持锁时外部路径仍能登记。
- 用临时目录和 oneshot/barrier 控制 cleaner-first 与 handoff-first；测试不使用 sleep、用户目录或真实媒体服务。

本切片不改变工具参数、IPC、配置、数据库 schema、asset ID 格式或文件保留期；无需数据或配置重置。

## 替代方案

- 所有 rich-path 输入一律持有 gate：拒绝。任意外部路径可位于慢速网络卷；让它阻塞 generated-media GC 和 producer 会扩大锁的影响面。只在 canonical parent 命中 cleaner 管理的直接根目录时同步。
- 只在同步 `register_path_asset` 外围取锁：拒绝。必须在 cleaner 快照前完成检查和登记；metadata/revalidation 也留在 permit 内，避免基于旧状态登记。
- 把 App 的 `CleanupRoots` 传入 Tools：拒绝。生产 generated-media 根已有稳定 common 配置函数；将 App 清理策略注入 Tools 会扩大契约并引入不必要装配耦合。
- 新增 registry reservation/待登记集合：拒绝。现有 gate 已提供所需互斥，额外状态会重复表达文件生命周期。
- 持锁到 MediaTool 或模型处理完成：拒绝。成功登记后 lease/TTL 已把保护责任交给 registry；长时间持锁只会推迟清理。

## 验证、影响与回滚

- 定向回归覆盖两个顺序：cleaner-first 时 handoff 在 gate 上等待，清理后 metadata 失败且无 session lease；handoff-first 时 cleaner 等待，lease 快照后保留文件。第三项回归确认外部 rich path 不等待该 gate。
- 通过：`cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`。定向测试：`cargo test --locked -p haven-tools file_media_handoff::tests`（3 passed）。
- 修改 `haven-tools` rich-path handoff/调用取消路径、registry gate 文档、架构文档与 ADR；没有新增 crate 依赖、公共 wire 或持久化变更。
- 回滚时删除 rich-path permit 获取和并发回归，并恢复原 handoff；没有数据迁移。回滚会重新允许 cleaner 的旧快照漏掉新登记的 lease。
