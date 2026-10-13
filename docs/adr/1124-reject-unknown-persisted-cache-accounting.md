# ADR 1124：拒绝未知的持久化 cache accounting 值

## 背景

`llm_usage.cache_accounting` 是 `inclusive`、`exclusive`、`unknown` 三值枚举。持久化读取过去复用了面向 provider/runtime 输入的宽容解析器，把拼写错误或未来未知值静默解释成 `unknown`。这会隐藏存储契约漂移，也与 `call_kind` 等持久封闭枚举的读取行为不一致。

## 决定

1. `CacheAccounting::parse_closed` 只接受 `inclusive`、`exclusive` 和 `unknown`，其他字符串返回 `None`。
2. `llm_usage.cache_accounting` 的 SQLite 读取使用严格解析；非法值作为列转换错误返回，错误指出表、字段和值。
3. 现有 `CacheAccounting::parse` 继续保留对外部/runtime 值的宽容语义：无法识别的值映射为 `Unknown`。写入适配器仍由各自入口校验其输入。

## 替代方案

- 继续静默映射为 `unknown`：会掩盖损坏数据和契约漂移。
- 将所有调用点改为严格解析：会改变 provider/runtime 兼容语义，并扩大与存储读取无关的行为范围。

## 影响

规范值及其存储格式不变；只有读取到非法持久化值时，`list_session_llm_usage` 从成功返回 `unknown` 改为返回错误。没有 schema 变化，不需要数据库重置。

## 验证

- 测试覆盖三种规范值、runtime 宽容回退，以及非法持久值读取失败。
- 运行 `cargo fmt --all -- --check`、`cargo test --locked -p haven-common`、`cargo test --locked -p haven-memory`、`cargo clippy --workspace --locked -- -D warnings` 和 ADR 索引检查。

## 回滚

回滚只恢复旧的宽容读取行为，不需要迁移。若存储中存在非法值，修复数据或重置数据库后也可恢复读取。
