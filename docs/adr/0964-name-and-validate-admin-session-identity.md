# ADR 0964：统一 Admin 会话结果身份字段

## 状态

已接受并实现（2026-10-11）。

## 背景

`haven_diagnostics` 的 `sessions` 与 `errors` 返回会话实体记录。Rust producer 将会话 ID 序列化为泛名 `id`，没有表达字段所指实体；UI 专用 renderer 也只要求该字段是字符串，因此非规范值可进入会话行的 key 和展示回退文本。

项目实体身份约定使用带实体语义的 `xxx_id` 字段名，并要求 Haven 实体 ID 为 `{prefix}-{uuid32}`。这两类记录的身份都来自持久会话，前缀为 `ses-`。

## 决定

- `SessionSummaryOutput` 与 `SessionErrorOutput` 的 JSON 身份字段统一为 `session_id`。
- Admin UI renderer 使用 `session_id` 作为行 key 和标题回退；`sessions` 与 `errors` 的每条记录都必须通过 canonical `ses-*` 校验。
- 缺少或无效的会话身份使 builtin 专用 renderer 回退为通用 JSON；title 仍可选，未知或无效的 session status 继续按现有校验回退。

## 替代方案

- 保留泛名 `id` 并只增加格式校验：仍会让 Rust 输出字段无法表明其会话实体语义。
- 接受任意非空字符串：无法保证 UI 行身份符合持久会话 ID 契约。

## 影响

`haven_diagnostics` 的模型可见 tool output 将字段从 `id` 改为 `session_id`，UI renderer 同步消费新字段。Rust DTO、Tauri IPC、数据库 schema、持久化数据和 session lifecycle 均未改变；无需数据重置。

## 验证

- Rust Admin output 测试断言 `sessions` 和 `errors` 序列化 `session_id`。
- UI 测试覆盖有效 canonical 会话 ID、错误前缀、非规范 UUID32 和缺失身份；畸形记录回退通用 JSON。
- `cargo fmt --all -- --check`
- `cargo test --locked -p haven-tools`
- `cargo clippy --workspace --locked -- -D warnings`
- `corepack pnpm run check`
- `corepack pnpm run test:run`
- `pwsh -NoProfile -File scripts/check-adr-index.ps1`
- `git diff --cached --check`

## 回滚

恢复两个 Rust DTO 的 `id` 字段、UI renderer 的旧 key 和旧 JSON fixture 即可。没有持久化或配置迁移。
