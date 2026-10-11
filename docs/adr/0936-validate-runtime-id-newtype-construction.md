# 0936：校验运行时实体 ID newtype 的构造入口

## 状态

已接受并实现（2026-10-11）。

## 背景

`id_newtype!` 原本给 `ConfirmId` 与 `RecordingId` 派生无验证的 `From<String>`、`From<&str>` 和 `Deserialize`。即使命令入口先调用 Common ID 校验器，其他调用方仍可把任意字符串变成带类型的 ID；Serde 也接受不符合 `conf-*` / `rec-*` 的数据。自动生成的 `pub` tuple 形状是项目既有 contract，但外 crate 不应绕过专用构造入口。

## 决定

- `ConfirmId::new()` 与 `RecordingId::new()` 分别调用 `new_id("conf")` 和 `new_id("rec")`。
- `TryFrom<String>` / `TryFrom<&str>` 通过 `is_canonical_id` 校验 type-specific 前缀；不再提供无验证的 string `From`。
- 为反序列化派生配置 `try_from = "String"`，输入在进入类型前执行同一验证；序列化仍输出原始字符串。
- 保持 `pub struct X(pub String)` 形状，并添加 `#[non_exhaustive]`，保留项目约定的 tuple 表示，阻止依赖 crate 直接构造。
- App confirmation command 在既有边界校验后使用 `TryFrom`；恢复的 Agent interaction 若含非法 confirmation ID，则记录告警并不安排 expiry worker。

## 影响

这是未发布 workspace Rust API 的破坏性收口。Serde wire 形状不变；非法 ID 现在会被拒绝，合法生成值不变。

## 验证

- 单元测试覆盖生成、字符串构造、非法前缀/格式拒绝及 JSON roundtrip/非法反序列化。
- 静态搜索确认没有无验证 `ConfirmId` / `RecordingId` string conversion。
- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked -- --test-threads=1`
- `cargo clippy --workspace --locked -- -D warnings`
- ADR 索引检查。

## 替代方案

- 保留 `From<String>` 并只依赖调用点自觉校验：拒绝，类型自身与 Serde 入口仍允许无效状态。
- 改成私有 named-field struct：拒绝，当前项目 ID newtype contract 明确采用 `pub struct X(pub String)`；`#[non_exhaustive]` 可阻止外部绕过构造，同时保留该表示。

## 回滚

如某个未来边界需要接收未验证原始值，应先保留为 `String` 并在该边界校验；不要恢复无验证的 `From<String>`。
