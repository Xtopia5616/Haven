# ADR 0687：缩短 Release 构建时间并保留内嵌 UI

## 状态

已采纳并实施。

## 背景

Release profile 使用全量 LTO（`lto = true`）和单个代码生成单元。Cargo 将依赖图中的跨 crate 优化集中到最终链接步骤，并限制单 crate 代码生成的并行度，增加桌面发布构建耗时。工作区已经按职责拆成多个 crate，开发配置也禁用了增量缓存以控制每个 worktree 的磁盘占用。

Tauri 的 `frontendDist` 指向 `ui/build`。发布应用需要继续包含构建后的 Svelte 前端资源。

## 决定

- Release 改用 ThinLTO（`lto = "thin"`），保留跨 crate 优化，同时减少全量 LTO 的链接成本。
- 将 Release `codegen-units` 调为 16，让 crate 代码生成有更多并行单元。
- 保留 `opt-level = 3`、`strip = true`、现有非增量缓存策略和 Tauri `frontendDist` 配置。
- 不把 Rust workspace crate 改成运行时 DLL。Cargo 已按 crate 缓存编译结果；额外 DLL 需要处理 Rust 版本绑定、Windows DLL 加载路径和安装包资源布局，不能仅靠 crate 拆分保证更快的增量编译。

## 替代方案

- 保留单代码生成单元与全量 LTO：拒绝，发布链接开销仍是当前配置主动增加的成本。
- 完全关闭 LTO：拒绝，ThinLTO 能保留跨 crate 优化，作为编译耗时与运行性能之间的折中。
- 分发 Rust crate DLL：拒绝，增加运行时部署与加载约束，且不会消除 Cargo 重新编译依赖 crate 的情形。

## 影响与验证

- 生产 Rust 编译和最终链接配置改变，应用逻辑、依赖版本、配置、IPC 和持久数据均不变，无需重置。
- `ui/build` 仍由 Tauri 的 `frontendDist` 随应用打包。
- `cargo fmt --all -- --check`、`cargo tauri build` 与 `git diff --check` 均通过；桌面构建生成 MSI 和 NSIS 安装包，`ui/build` 已随 Tauri 生产构建完成。
- 本轮 Release 首次冷缓存构建耗时 9 分 12 秒；没有旧 profile 下的同机对照数据，因此不声称已测得具体提速比例。构建时长仍受机器、缓存与链接器影响。

## 回滚

恢复 `lto = true` 和 `codegen-units = 1` 即可；无需数据或配置迁移。
