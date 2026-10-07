# ADR 0662：删除 Gemini 响应中的重复 Serde alias

## 背景

`GeminiCandidate.finish_reason` 的 Rust 字段名和默认 Serde 名都是 `finish_reason`，但字段还声明了同名 alias。这个 alias 不扩大可接受的 JSON 字段集合，也不映射上游协议的另一种名称。Gemini 标准响应使用 `finishReason`，该 camelCase alias 是实际 wire 映射，需要保留。

## 决定

- 删除 `finish_reason` 自身同名的 Serde alias。
- 保留 `finishReason` 映射，以及其它与 Gemini 上游 JSON 命名不同的别名。
- 将规则补入项目命名规范：Serde alias 只用于表达真实 wire 名称差异；Haven 内部旧字段名不作为隐式兼容入口。
- 无持久化字段、数据库 schema 或配置变化，无需重置。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --locked -p haven-llm`
- `cargo clippy --locked -p haven-llm -- -D warnings`

## 回滚与重置

回滚仅恢复多余的 attribute；对当前 Serde 名和 Gemini camelCase wire 字段均无行为影响。不需要数据或配置重置。
