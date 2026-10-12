# ADR 1050：只将规范实体 ID 视为持久身份

## 状态

已接受（2026-10-12）

## 背景

上传批次、暂存目录和受管媒体文件名中嵌入 `file-*` ID。清理器各自检查 hex 字符，但允许大写 A–F，与 Common 的 canonical ID 定义不一致。UI 续接按 `msg-` 前缀判断用户消息是否持久化，resume 合并也按 `step-` 前缀保留工具卡片；格式错误的值会被误认为耐久身份。

## 决定

- App 媒体文件、上传批次和暂存目录识别器复用 `haven_common::types::is_canonical_id` 校验 `file-*` 段。
- UI 续接只把规范 `msg-*` 视为已持久化用户消息；resume 只把规范 `step-*` 视为可保留的持久工具/Ask 卡片。
- 本地流式项仍按其运行时状态保留；只有判断其持久身份的分支要求 canonical ID。

## 影响与兼容

由 Haven 生成的名称与身份均符合此规则。格式错误或大写 ID 不再进入清理识别/持久卡片分类。IPC shape、数据库 schema、provider ID 及合法 ID 均不变，不需要迁移或重置。

## 验证

- Rust 单测覆盖受管媒体文件、上传批次和暂存目录：接受规范小写 ID，拒绝大写 hex 与非法 hex。
- UI 单测覆盖续接时短/大写 `msg-*` 不会被视为持久身份，恢复合并时短/大写 `step-*` 卡片不按持久项保留。
- `cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked -- --test-threads=1`、`corepack pnpm run check` 与 `corepack pnpm run test:run`。
