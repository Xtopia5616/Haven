# ADR 0001：固定可重复的质量基线

日期：2026-08-26  
状态：已采纳

## 背景

Haven 的 Rust 工具链此前未固定，CI 仅覆盖 Linux，且严格 Clippy 与覆盖率上传允许静默继续。这样会使同一改动在本机和 CI 得到不同结论，特别是 Windows 宿主边界无法获得持续验证。

## 决定

- 以 `rust-toolchain.toml` 固定 Rust 1.98.0，并声明 `clippy`、`rustfmt` 组件；CI 显式使用同一版本。
- CI 在 Linux 执行格式化、检查、严格 Clippy；在 Linux 与 Windows 执行 workspace 测试。
- UI 使用 `.node-version` 固定在 Node 24.20.0、使用 `ui/package.json` 固定 pnpm 11.24.0，并在 Linux 与 Windows 执行类型检查、测试和生产构建；仅 Linux 上传构建产物。
- UI workspace 在 SvelteKit 仍声明 `cookie ^0.6.0` 时，通过 `ui/pnpm-workspace.yaml` 固定到已修复安全问题的 `cookie 0.7.2`；待上游依赖范围更新后应删除该 override 并重新审计。
- 覆盖率生成和上传失败均使 CI 失败，不使用 `continue-on-error` 掩盖结果。

## 替代方案

继续使用每次运行时可变的 stable 工具链，或只依赖 Linux CI。前者不能保证 lint 规则和编译器诊断一致，后者不能持续发现 Windows 专属回归，均被拒绝。

## 影响

开发者须安装匹配的 Rust 工具链、`.node-version` 指定的 Node 24.20.0 和 `ui/package.json` 指定的 pnpm 11.24.0。升级工具链、Node 或 pnpm 版本需单独变更对应版本文件、CI 与本 ADR（或新增替代 ADR），并重新通过所有门禁。

## 验证

`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked -- --test-threads=1`、`corepack pnpm --dir ui run check`、`corepack pnpm --dir ui run test:run` 与 `corepack pnpm --dir ui run build`。

## 回滚与重置

回滚本决策只需恢复前一工具链与 CI 配置，不影响用户数据，无需重置 Haven 数据目录。
