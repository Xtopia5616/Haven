# 依赖与工具链更新审计（2026-10-14）

## 范围与结论

本轮核对 Rust、Node.js、pnpm、UI 直接依赖与 Cargo/npm 锁文件，并检查了 CI 中对应的安装与构建动作。升级以已发布的兼容版本和可验证的安全修复为依据；未把 SvelteKit 3 或其他跨主版本迁移混入本轮。

工具链现固定为 Rust 1.99.0、Node.js 24.21.0、pnpm 12.12.1。Rust 1.99.0 于 2026-10-01 发布；Node 24 仍是 LTS 主版本。pnpm 12.12.1 可从 npm registry 安装，并包含 12.12.0 的构建权限安全修复。pnpm 官方发布页已列出 12.13.0，但本轮检查时 npm registry 对该版本返回未发布，因此暂不固定到该版本，以保证 Corepack 与 CI 可重复安装。[Rust 发布记录](https://blog.rust-lang.org/2026/10/01/Rust-1.99.0/) · [Node 发布状态](https://nodejs.org/en/about/previous-releases) · [pnpm 12.0](https://pnpm.io/blog/releases/12.0) · [pnpm 12.12.0 安全修复](https://pnpm.io/blog/releases/12.12.0)

## 本轮升级

| 范围 | 更新 | 原因 |
|---|---|---|
| Rust 工具链 | 1.98.0 → 1.99.0 | 使用当前稳定工具链；仓库固定版本、CI 与开发说明保持一致。 |
| Node.js | 24.20.0 → 24.21.0 | 更新 Node 24 LTS 补丁版本；`.node-version`、UI 引擎约束和 CI 安装来源保持一致。 |
| pnpm | 11.24.0 → 12.12.1 | 更新到可从 npm registry 获取的 pnpm 12；根/UI `packageManager` 和 CI 一致。pnpm 12 使用 Rust 重写的客户端；本轮没有启用实验性的 loaded node linker。 |
| UI 直接依赖 | `@sveltejs/vite-plugin-svelte` 7.3.1、`@tauri-apps/api` 2.12.3、`jsdom` 30.1.2、`markdown-it` 15.0.2、`prettier` 3.9.10、`svelte` 5.57.2、`vite` 8.3.4 | 锁定到核对时可用的兼容更新；Vitest、SvelteKit 和 adapter-static 保持当前主版本。 |
| npm 传递依赖 | `devalue` 5.9.3、`source-map-js` 1.2.2 | 通过 pnpm overrides 固定到包含已知安全修复的版本。 |
| Rust 传递依赖 | `h2` ≥0.4.16、`quinn-proto` ≥0.11.18、`rustls` ≥0.23.46、`time` ≥0.3.47、`event-listener` 5.4.2、`chacha20` 0.10.2、`async-trait` 0.1.92；更新 Tauri、reqwest、tokio、serde_json、uuid、tract、tiktoken-rs 等兼容依赖 | 更新 Cargo.lock 中的兼容版本并消除本轮发现的已知漏洞；`async-trait` 0.1.92 也修复了 Rust 1.99 Clippy 的重复 `must_use` lint（[上游发布记录](https://github.com/dtolnay/async-trait/releases)）。 |
| XML 解析 | `quick-xml` 声明从 0.38 更新到 0.41；锁文件解析为兼容的 0.42.0 | 修复未受信任 XML 声明及重复属性处理相关漏洞。无需调用新 API，安全改进随依赖更新生效。 |
| WebView2 绑定 | `webview2-com` 0.38.2 → 0.39.1 | 新版 Tauri 已依赖 0.39.1；保留旧直接依赖会同时加载两套 Windows 接口类型并导致 `windows_core` trait 不兼容，因此将 Haven 的直接依赖对齐到同一版本。 |
| CI | checkout v7、upload-artifact v6、pnpm/setup v3；Rust 与 tarpaulin 版本同步 | 将 CI 的安装和构建工具与本地固定版本对齐；`pnpm/setup` 负责按 `.node-version` 安装 Node、pnpm 并执行冻结锁文件安装（[官方 action 文档](https://github.com/pnpm/setup)）。 |

`markdown-it` 使用 `linkify: true` 处理模型输出；升级到 15.0.2 修复该解析路径上的拒绝服务问题，也包含 smartquotes 的复杂度修复。[markdown-it 更新记录](https://github.com/markdown-it/markdown-it/blob/master/CHANGELOG.md)

## 安全审计

- 更新前 `pnpm audit` 报告 `markdown-it`、`devalue`、`source-map-js` 与 `undici` 漏洞。本轮更新/覆盖后，npm audit 返回 0 个 critical/high/moderate/low/info 漏洞。对应修复版本见 [markdown-it 15.0.2](https://github.com/markdown-it/markdown-it/blob/master/CHANGELOG.md)、[devalue GHSA](https://github.com/advisories/GHSA-j22f-vq7h-c4qm)、[source-map-js GHSA](https://github.com/advisories/GHSA-68fv-2mgg-jv7q) 和 [undici GHSA](https://github.com/nodejs/undici/security/advisories/GHSA-rfgv-xxqx-mfg5)。
- 更新前 `cargo audit` 报告 `h2`、`quick-xml`、`quinn-proto`、`rustls`、`time` 等漏洞。本轮更新后无已知漏洞报告。参考 [h2](https://rustsec.org/advisories/RUSTSEC-2026-0258)、[quick-xml 0194](https://rustsec.org/advisories/RUSTSEC-2026-0194)、[quick-xml 0195](https://rustsec.org/advisories/RUSTSEC-2026-0195)、[quinn-proto](https://rustsec.org/advisories/RUSTSEC-2026-0338.html)、[rustls](https://rustsec.org/advisories/RUSTSEC-2026-0285.html)、[time](https://rustsec.org/advisories/RUSTSEC-2026-0009.html)。
- `cargo audit` 仍显示 3 条 warning、未报告漏洞：`paste` 与 `proc-macro-error` 已停止维护；`glib 0.18.5` 存在 `VariantStrIter` soundness advisory。它们是现有依赖链，不属于本轮新增；后续应结合反向依赖和目标平台单独评估替换或升级。

## 暂不升级与新特性评估

- SvelteKit 3 属于需修改配置、应用 API 与构建约束的主版本迁移，留待单独迁移任务；本轮维持 SvelteKit 2 与 adapter-static 3。
- `cpal`、`rusqlite`、`thiserror`、`zip` 等可用更新涉及 API 或主版本变更，未发现必须伴随当前安全修复升级的要求，暂缓以控制回归范围。
- Rust 1.99 与本次库更新没有要求 Haven 新增代码调用的新稳定 API。pnpm 12 与 `quick-xml` 的安全修复由工具链/依赖升级直接获得；未启用实验特性，也未发现需要为“使用新特性”而改写现有代码的理由。

## 验证记录

验证结果：

- Rust 1.99.0 下 `cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings` 均通过。
- `cargo audit` 未发现漏洞；保留前述 3 条已有 warning。`pnpm audit --registry=https://registry.npmjs.org` 未发现漏洞。
- `pwsh -NoProfile -File scripts/check-crate-dependencies.ps1`、`check-adr-index.ps1`、`check-ipc-events.ps1` 通过；`check-ipc-contracts.ps1` 通过，核对 81 个 handler。Windows 本机首次 codegen 链接因 MSVC PDB 大小限制失败；以临时环境变量 `CARGO_PROFILE_DEV_DEBUG=0` 重跑后通过，仓库 profile 未改动。
- UI 使用 pnpm 12.12.1 验证：`pnpm install --frozen-lockfile`、`pnpm run check`（0 error/0 warning）、`pnpm run test:run`（153 个文件、1,287 项通过）和 `pnpm run build` 均通过。本机 Node 是 24.20.0；CI 按 `.node-version` 安装 24.21.0。
- `cargo test --workspace --locked -- --test-threads=1` 在现有未提交的 `haven-skills` serde-saphyr 工作中有 2 项失败：`parse_unsupported_language_preserved` 与 `registry_listing_keeps_valid_names_unique_across_invalid_diagnostics`。未改动该用户工作；`cargo test --workspace --exclude haven-skills --locked -- --test-threads=1` 全部通过。两次本机 Windows 测试均设置 `CARGO_PROFILE_TEST_DEBUG=0` 以避免 MSVC PDB 上限。
