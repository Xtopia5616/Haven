# ADR 1318：区分开发窗口与正式版实例

## 状态

已接受并实现（2026-10-18）。

## 背景

`cargo tauri dev` 与已安装的 Haven 使用相同窗口标题和 Tauri identifier。Windows 单实例插件据此把第二次启动转发到第一个进程，无法同时保留开发窗口和正式版窗口。两种运行方式也默认共用 `%APPDATA%\haven`，会话和配置容易混在一起。

## 决定

- 开发启动使用 `tauri.dev.conf.json` 中独立的 `com.haven.app.dev` identifier，让开发版与正式版各自保持单实例并能并行运行。
- 开发窗口的原生标题和应用内标题栏显示“开发版”；生产构建继续显示“Haven”。
- 根目录 `pnpm run dev` 设置 `HAVEN_DATA_DIR` 为 `%APPDATA%\haven-dev`（非 Windows 为 `~/.local/share/haven-dev`）。`ConfigLoader::data_dir()` 是该显式覆盖值的唯一读取入口；所有衍生持久路径继续从它计算。
- 直接运行 `cargo tauri dev` 时，开发者需同时传入开发 Tauri 配置并设置 `HAVEN_DATA_DIR`。

## 影响与数据

开发环境首次启动会在新目录创建独立配置与数据库，不会复制正式版凭据、会话、记忆、日志、技能或媒体，也不迁移或修改正式版数据。无需重置正式版数据。开发与正式版窗口可同时运行。

## 替代方案

- 只增加“开发版”视觉标签：拒绝，单实例插件仍会把第二次启动转发到已有 Haven。
- 只改 Tauri identifier：拒绝，开发版与正式版仍会共享配置和数据库路径。

## 验证

- `cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings` 均通过。
- `cargo test --workspace --locked -- --test-threads=1` 与 `cargo test --locked -p haven-common -p haven-app-binary -- --test-threads=1` 均通过。
- UI `check`、`test:run`（161 个文件 / 1340 项）和生产 `build` 均通过。
- `node scripts/dev-tauri.ts --help` 启动器检查、ADR 索引检查（1301 条）与 `git diff --check` 通过。
- 本轮未执行安装包与开发窗的人工并行启动验收；需在本机确认原生标题与两份数据目录。
