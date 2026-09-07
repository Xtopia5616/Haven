# Haven

Haven 是一个面向 Windows 的本机语音助手。后端使用 Rust/Tauri 2，界面使用 Svelte 5；会话由可恢复的 ReAct 循环编排，本机工具受统一安全确认策略保护。

> 当前为测试版本。重构期间不承诺旧数据库、配置或 ReAct 快照兼容；升级前请阅读 [发布与重置说明](docs/release-and-reset.md)。

## 开发环境

- Windows 10/11（桌面应用的运行与打包目标）
- Rust 1.98.0，由 `rust-toolchain.toml` 固定；需包含 `clippy`、`rustfmt`
- Node.js 24.20.0 与 pnpm 11.24.0；版本分别由 `.node-version` 和 `ui/package.json` 固定
- UI 类型检查使用 TypeScript 7.0.2 原生编译器；Svelte 兼容层保留 TypeScript 6.0.3
- Tauri 的 Windows 前置依赖：Microsoft C++ Build Tools 和 WebView2 Runtime

首次安装依赖：

```powershell
corepack pnpm --dir ui install --frozen-lockfile
cargo fetch --locked
```

## 运行

仅启动前端开发服务器（浏览器预览，不包含 Rust/Tauri 后端）：

```powershell
corepack pnpm --dir ui run dev
```

浏览器预览中的状态按钮会明确显示“浏览器预览”；这是正常的无后端模式，不能调用
Tauri 命令。需要完整功能时请运行下面的桌面开发命令。

桌面开发推荐使用根目录的一键命令，它会同时启动 Vite 前端和 Rust/Tauri 后端：

```powershell
corepack pnpm run dev
```

桌面开发命令会由 Tauri 负责启动 Rust 后端，并通过同一个 `4721` 端口使用 Vite。
如果浏览器预览已经先启动，UI 入口会复用现有的 Haven Vite 进程，不会再启动第二个
前端服务；如果端口被其它程序占用，会给出明确错误。

也可以直接使用 Tauri CLI。若尚未安装，可执行 `cargo install tauri-cli --version "^2"`，随后运行：

```powershell
cargo tauri dev
cargo tauri build
```

`cargo tauri build` 会先执行 `corepack pnpm --dir ui run build`，不依赖开发服务器；可在
Vite 开发服务器运行时执行。产物在 `target/release/bundle/`。请勿把 API 密钥提交到仓库；
应用配置保存在用户数据目录。

## 质量检查

执行完整本地门禁：

```powershell
cargo fmt --all -- --check
cargo check --workspace --locked
cargo clippy --workspace --locked -- -D warnings
cargo test --workspace --locked -- --test-threads=1
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
corepack pnpm --dir ui run build
```

Rust 测试应使用内存数据库或唯一临时目录，不能读写真实用户配置、AppData 或访问网络。单个 crate 的测试命令见 [AGENTS.md](AGENTS.md)。

## 文档入口

- [架构与 crate 边界](docs/architecture.md)
- [开发与架构治理规范](docs/development-standards.md)
- [Git 提交流程](docs/git-workflow.md)
- [稳定性重构计划](docs/stability-refactor-plan.md)
- [重构实施手册](docs/refactor-execution-guide.md)
- [发布与数据重置](docs/release-and-reset.md)
- [架构决策记录](docs/adr/README.md)

## 故障报告

报告问题时请提供操作系统版本、Haven 版本、复现步骤、预期与实际行为。可以附上经过脱敏的日志片段；不要提交 API 密钥、完整对话、文件路径或工具命令输出。
