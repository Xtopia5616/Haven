# ADR 1049：集中构造 Tools 的实体 ID schema

## 状态

已接受（2026-10-12）

## 背景

Builtin tool JSON schema 在多个文件里重复硬编码 `asset`、`ses`、`msg` 和 `toolrun` ID pattern。重复 pattern 可能与 Common 的 `is_canonical_id` 漂移；另外 JSON Schema 正则的 `$` 可能在尾随换行前匹配，从而比 Common 接受更多输入。

## 决定

- 由 Tools 私有 `builtin::tool_schema::canonical_entity_id_schema` 构造 Haven 实体 ID schema；不把 JSON Schema 依赖或实现放进 Common。
- builder 生成固定前缀的 pattern，并设置相同的 `minLength` / `maxLength`；调用点可提供原有字段描述。
- 工具执行时的语义校验继续使用 Common canonical ID checker。

## 影响与兼容

有效 ID 与描述文本不变；schema 新增精确字符串长度约束，拒绝 Common 本已拒绝的额外尾随字符。不会改变持久化数据、IPC shape 或 provider ID。无迁移或重置要求。

## 验证

- schema validator 对 `asset`、`ses`、`msg`、`toolrun` 的成功、短 ID、大写 hex、尾随换行和错误前缀逐项与 `is_canonical_id` 对照。
- `cargo fmt --all -- --check`、`cargo test --locked -p haven-tools`、`cargo check --locked -p haven-tools` 与 `cargo clippy --locked -p haven-tools -- -D warnings`。
