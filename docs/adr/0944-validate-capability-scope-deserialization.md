# 0944：反序列化时校验 CapabilityScope

## 状态

已接受并实现（2026-10-11）。

## 背景

`CapabilityScope::try_new` 拒绝空 key、周围空白、冒号分隔和空层级；但派生的 `Deserialize` 会直接绕过它。授权匹配按点号 ancestry 继承，因此形如 `files..delete` 的非法 key 不应进入请求或确认 receipt。

模型工具请求另有前置边界：Agent 在 admission 前按已发布的工具 schema 校验参数，校验失败的调用不会进入授权 hook；因此运行时 `permission_key(tool_name, params)` 不需要对非法动态 `scope` / `operation` 猜测父级或使用宽泛 fallback。

## 决定

- `CapabilityScope` 保持透明字符串序列化，反序列化改为调用 `try_new` 并拒绝不规范值。
- 配置与 SQLite grant 读取继续使用既有严格解析；运行时 `new` / `From` 仅供可信的规范 policy key 构造路径使用。
- 不为非法动态 route 值生成降级 capability；schema 校验失败直接阻止 admission。

## 影响

有效 capability wire 值与授权匹配行为不变。含非规范 capability 的 JSON/replay 输入现在会在反序列化边界失败，不会形成父级 grant。SQLite grant 已经在读取时严格校验，无需数据库重置。

## 验证

- Common 单元测试覆盖有效 round-trip 形状及空值、空白、冒号和空层级拒绝。
- Agent tool batch 在授权前执行 catalog schema 校验；已有 invalid enum/input 测试覆盖失败分支。
- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- ADR 索引及 `git diff --check`。

## 替代方案

- 保留派生 `Deserialize` 并依赖每个消费者再次校验：拒绝。容易让新的 event/IPC consumer 忘记检查，使 typed 值在授权 ancestry 计算前失去规范保证。
- 将非法 route 值映射到工具根 capability：拒绝。根 allow 可能继承到无关操作。

## 回滚

若需要读取非规范外部数据，应为其定义一次性迁移或独立 raw-wire DTO；不要恢复对 `CapabilityScope` 的无校验反序列化。
