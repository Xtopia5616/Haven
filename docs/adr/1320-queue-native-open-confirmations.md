# ADR 1320：为原生目录与外链打开接入 AppCommand 确认

状态：已接受并实施（2026-10-18）。

## 背景

`open_external` 与 `open_skills_dir` 都通过 `AuthorizationPort` 判断权限，但在 `RequiresConfirmation` 分支直接返回错误。已有 AppCommand confirmation owner 无法接续这两种原生打开操作，用户因此只看到操作失败，无法在权限确认弹窗中授权。

## 决定

1. 两个命令遇到 `RequiresConfirmation` 时，把经过校验的目标、原始授权请求和 receipt 放入现有 AppCommand confirmation queue。批准后重新校验 receipt 与目标安全性，再执行原生打开。
2. 外链 URL、本地路径与 Skills 目录路径只保留在后端 pending action 和授权请求中；renderer 只收到不含目标值的脱敏操作摘要。
3. `Blocked` 仍然失败。网络策略 `deny` / `restricted`、Plan、明确拒绝和沙箱边界不能被一次确认覆盖。
4. 前端将确认待处理视为已排队状态，不再显示“打开失败”提示。
5. 审计范围：其它 AppCommand 授权入口已有确认队列。CodeMode/Workflow 嵌套调用在需要单独确认时仍 fail closed，并提示改用 Legacy 单工具调用；当前确认生命周期无法暂停并恢复嵌套 continuation，见 ADR 1163 与 ADR 1166。

`open_external` 使用 `NetworkAccess::Opaque`，因此 `deny` / `restricted` 下的硬拒绝不会弹确认；切换到“请求确认”后，普通确认路径会进入本 ADR 接入的弹窗。硬拒绝错误现在保留授权引擎给出的策略原因，便于诊断。

## 替代方案

- 忽略 `RequiresConfirmation` 或改为自动批准：拒绝，会绕过当前权限策略。
- 将 `Blocked` 也转换成确认请求：拒绝，会让确认弹窗覆盖硬安全边界。

## 影响与恢复

复用现有 AppCommand confirmation 生命周期，不新增持久状态、数据库字段或 IPC payload。Rust 命令的安全元数据与 IPC 文档标注其确认行为。回退时移除两个 pending action 与调用端队列分支即可；不需要数据重置。

## 验收

- `cargo fmt --all -- --check`：通过。
- `cargo check --workspace --locked`：通过。
- `cargo clippy --workspace --locked -- -D warnings`：通过。
- `cargo test --workspace --locked`：通过。
- `corepack pnpm run check`（在 `ui/`）：通过，0 错误、0 警告。
- `corepack pnpm --dir ui run test:run`：通过，161 个测试文件、1341 个测试。
- `corepack pnpm --dir ui run build`：通过。
- `scripts/check-ipc-contracts.ps1`：通过，89 个 handler 契约一致。
