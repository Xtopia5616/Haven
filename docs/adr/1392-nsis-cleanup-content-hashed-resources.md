# ADR 1392：修复 Windows NSIS 升级与卸载的哈希静态资源残留

## 状态

已采纳并实施。Windows 发布 Gate 仍 Open。

## 背景

Tauri NSIS bundle 将 `ui/build` 复制到安装目录的 `ui`。SvelteKit 为静态资源生成内容哈希文件名；NSIS 自动生成的卸载脚本只枚举当前构建的文件。跨版本升级不会删除已经消失的旧哈希文件，卸载当前版本时也不知道旧文件的名称，导致安装目录在卸载后残留 `ui/_app`。

## 决定

- 配置 NSIS `installerHooks`，在安装前递归删除应用自有的 `$INSTDIR\ui`，再由安装器复制当前构建的资源；卸载前也递归删除该目录。安装目录内的 `ui` 只由 Haven 的打包资源使用，用户配置和数据库位于独立数据目录。
- Tauri 把 PREINSTALL/PREUNINSTALL hook 放在内建运行进程检查之前，因此两个 hook 都先调用同一个 `CheckIfAppIsRunning` 宏，再删除 UI 资源；Tauri 后续的内建检查保留。这样静默安装沿用 NSIS 原有的结束应用行为，交互安装若用户取消结束应用则先中止，不会先删资源。

## 替代方案

- 依赖生成的逐文件 `Delete`：拒绝。该列表只覆盖当前 bundle，无法发现旧版内容哈希名称。
- 对 `$INSTDIR` 整体递归删除：拒绝。安装根目录还包含可由 NSIS 明确管理的其他资源和元数据；清理范围限制在应用拥有的 `ui` 目录。
- 用通配符只删当前扩展名：拒绝。旧资源位于多层子目录，且未来静态文件类型可能变化。

## 影响、安全与重置

升级时，旧前端资源会在新资源复制前清除；卸载时清除完整前端树。Tauri 的进程检查先确保 Haven 不在使用这些文件。此改动不改变应用运行逻辑、配置、IPC、数据库 schema 或用户数据目录；无需数据迁移或重置。回滚时移除 `installerHooks` 配置和 `nsis-hooks.nsh`，然后重新构建安装包；届时会恢复旧版静态资源残留行为。

## 验证与限制

2026-10-24，在 Windows 11 专业版 Insider Preview `10.0.29683`、Rust 1.99.0、Node 24.21.0、pnpm 12.12.1、Tauri CLI 2.11.4、NSIS 3.12 上验证。数据库 schema 为 v61。使用未跟踪的 acceptance 版本覆盖将包版本设为 `0.1.2`；仓库配置中的应用版本仍为 `0.1.0`。

- `cargo tauri build --config target/acceptance-build-2026-10-24/tauri-0.1.2.json --bundles nsis --no-sign` 成功。产物 `target/release/bundle/nsis/Haven_0.1.2_x64-setup.exe` 大小 26,570,611 字节，SHA-256 `ccd62a706529052b88eff8dd01c8b3089c67a3afcd991a30a1d7eafd872ea9d7`。
- 在隔离 profile 中安装 0.1.0 再升级到 0.1.2，两安装器均退出码 0。升级前的 75 个旧资源被替换；升级后 75 个安装文件与当前 `ui/build` 完全一致，缺失 0、残留 0。
- 另一次升级时 0.1.0 app 进程确实运行；0.1.2 静默安装成功，结束旧进程并安装文件版本 0.1.2。之后静默卸载退出码 0，完整安装目录和 `ui` 均不存在。
- 卸载后隔离数据目录中的 `config.toml`（4655 字节）与 `haven.db`（4096 字节）保留；SQLite 只读 `PRAGMA quick_check` 返回 `ok`，包含 1 个 session 和 8 条 message。
- 此 ADR 只签核 NSIS 安装、升级、卸载及数据保留这部分。工具确认通过路径、Schedule/后台任务、durable UI 丢失事件补齐、rollback 与恢复并发和物理磁盘 ENOSPC 尚未全部验收；最终发布 Gate 继续按路线图 §5.1 保持 Open。
