# 1038：拒绝有歧义的 LLM 配置查找键

## 状态

已采纳并实施（2026-10-12）。

## 背景

Provider 名、ModelConfig ID 和 RequestKind 分别用于 first-match 查找或路由选择。UI 对部分键有唯一性校验，但 TOML 加载与 Settings 写入的共享后端语义校验不完整；重复项会使显示、保存和实际选中对象不一致。

## 决定

- 公共配置校验统一拒绝空 Provider 名、重复 Provider 名、重复 ModelConfig ID 和重复 RequestKind policy。
- 相同语义校验应用于 TOML 加载和 Settings 更新；用户界面校验只用于即时反馈，不能替代后端 owner。
- 不对列表做“首个胜出”或静默去重，避免改变用户配置所指向的模型/服务。

## 影响

数据库 schema、合法配置和 wire shape 不变，无需数据库重置或迁移。包含歧义键的 TOML 在加载时按既有坏配置恢复规则处理；通过 Settings 提交的歧义配置在写入前失败。provider model ID、外部 API model 名和配置记录 ID 仍是不同概念；用户只需在源 TOML 中消除重复键即可恢复配置加载。

## 验证

- Common 单元测试覆盖三类重复键在 AppConfig 与 Settings 的一致拒绝。
- TOML loader 测试覆盖重复 Provider 名的恢复行为。
- `cargo test --locked -p haven-common`
- `cargo fmt --all -- --check`、`git diff --check`

## 回滚

若某类配置键允许重复，必须先替换对应 first-match 查找为确定的多值语义，并同步 UI/文档；不得只移除校验而保留含糊查找。
