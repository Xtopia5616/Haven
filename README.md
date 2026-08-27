# Haven

Haven 是一个面向 Windows 的本机语音助手。后端使用 Rust/Tauri 2，界面使用 Svelte 5；会话由可恢复的 ReAct 循环编排，本机工具受统一安全确认策略保护。

> 当前为测试版本。重构期间不承诺旧数据库、配置或 ReAct 快照兼容；升级前请阅读 [发布与重置说明](docs/release-and-reset.md)。

## 开发环境

- Windows 10/11（桌面应用的运行与打包目标）
- Rust 1.98.0，由 `rust-toolchain.toml` 固定；需包含 `clippy`、`rustfmt`
- Node.js 24.19.0 与 pnpm 11.24.0；版本分别由 `.node-version` 和 `ui/package.json` 固定
- Tauri 的 Windows 前置依赖：Microsoft C++ Build Tools 和 WebView2 Runtime

首次安装依赖：

```powershell
corepack enable
pnpm --dir ui install --frozen-lockfile
cargo fetch --locked
```

## 运行

前端开发服务器：

```powershell
pnpm --dir ui run dev
```

桌面开发与打包使用 Tauri CLI；如果尚未安装，可执行 `cargo install tauri-cli --version "^2"`，随后运行：

```powershell
cargo tauri dev
cargo tauri build
```

`cargo tauri build` 会先执行 `pnpm --dir ui run build`，产物在 `target/release/bundle/`。请勿把 API 密钥提交到仓库；应用配置保存在用户数据目录。

## 质量检查

执行完整本地门禁：

```powershell
cargo fmt --all -- --check
cargo check --workspace --locked
cargo clippy --workspace --locked -- -D warnings
cargo test --workspace --locked -- --test-threads=1
pnpm --dir ui run check
pnpm --dir ui run test:run
pnpm --dir ui run build
```

Rust 测试应使用内存数据库或唯一临时目录，不能读写真实用户配置、AppData 或访问网络。单个 crate 的测试命令见 [AGENTS.md](AGENTS.md)。

## 文档入口

- [架构与 crate 边界](docs/architecture.md)
- [开发与架构治理规范](docs/development-standards.md)
- [稳定性重构计划](docs/stability-refactor-plan.md)
- [重构实施手册](docs/refactor-execution-guide.md)
- [发布与数据重置](docs/release-and-reset.md)
- [架构决策记录](docs/adr/README.md)

## 故障报告

报告问题时请提供操作系统版本、Haven 版本、复现步骤、预期与实际行为。可以附上经过脱敏的日志片段；不要提交 API 密钥、完整对话、文件路径或工具命令输出。
