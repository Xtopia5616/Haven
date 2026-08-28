# ADR 0018：统一 VS Code 与 Tauri 的工具链入口

## 背景

项目固定了 Node.js 与 pnpm 的推荐版本，但 VS Code 集成终端使用的是进程启动时
继承的 PATH。因而裸 `pnpm` 可能指向其它版本；当 `engines` 使用固定版本下限时，
pnpm 会在执行任何 UI 命令前直接退出。Tauri 的 `beforeDevCommand` 与
`beforeBuildCommand` 也曾调用裸 `pnpm`，使同一问题阻断桌面开发和打包。

## 决定

- `ui/package.json` 的 `packageManager` 继续精确指定 pnpm 11.24.0，`.node-version`
  继续指定 Node.js 24.20.0；这两个文件仍是推荐工具链的唯一选择来源。
- `engines` 只声明可工作的 Node.js 24 和 pnpm 11 兼容范围，不把本机补丁版本差异
  变成硬阻断。
- 仓库根目录维护的 UI 命令统一使用 `corepack pnpm --dir ui ...`，由 Corepack
  解析 `packageManager`；Tauri hook 使用 `corepack pnpm ...`，因为 Tauri 会在
  `ui` 工作目录执行 hook，避免重复拼接 `ui` 路径。
- `.vscode/tasks.json` 提供 UI、Rust 和 Tauri 的可发现任务；Rust 任务仍由
  `rust-toolchain.toml` 选择 Rust 1.98.0，VS Code 工作区补充用户级 Cargo 路径。
- VS Code 的 TypeScript 服务使用 UI 工作区中的 TypeScript 6 兼容层；UI 的显式
  `check` 脚本继续使用 TypeScript 7 原生检查入口。

## 影响

在已安装 Node.js 24、pnpm 11 的机器上，旧版本的裸 pnpm 不再阻止命令启动；需要
完全可复现的检查时，Corepack 仍会使用声明的 pnpm 11.24.0。Rust 仍需安装 rustup
并让 `cargo` 可被 VS Code 找到；`rust-toolchain.toml` 会负责选择项目版本。此变更
不修改数据库、配置或用户数据。

## 验证

- `corepack pnpm --dir ui install --frozen-lockfile --offline`
- `corepack pnpm --dir ui run check`
- `corepack pnpm --dir ui run test:run`
- `corepack pnpm --dir ui run build`
- VS Code 的 `Haven: Rust check`、`Haven: Tauri dev` 与 `Haven: Tauri build` 任务
  使用仓库根目录作为工作目录。

## 回滚

回退 `ui/package.json`、`crates/app-binary/tauri.conf.json`、`.vscode/`、相关文档
和本 ADR；无需数据重置。
