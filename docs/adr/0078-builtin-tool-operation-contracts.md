# ADR 0078：内置工具按操作分支的输入与输出契约

## 背景

多个内置工具把互不相干的操作参数放在同一个宽松对象中。模型容易把参数发给错误操作，直到执行阶段才得到模糊错误；原生调用也可能绕过 JSON schema 的数值边界。不同操作的成功结果还缺少稳定的操作标识，UI 和恢复逻辑只能依赖字段猜测。

## 决定

- 对音频、剪贴板、进程、窗口、输入、系统、HTTP、后台动作、定时动作、记忆、跨会话 Agent 等分组工具，用 `oneOf` + `const` 将每个操作与其参数、必填字段和 `additionalProperties=false` 绑定。
- 将时间、数量、PID、音量、滚轮步数、能力列表等边界写入 schema；对 HTTP 超时和 PID 再在原生入口执行边界保护，避免内部调用绕过 schema 后产生未定义行为。
- 成功的结构化结果携带稳定的 `operation` 字段；系统结果额外携带实际 `scope`。不把请求 URL、凭据或完整敏感输入复制到结果中。
- 维持现有模型可见工具名称，不新增兼容别名；工具注册、权限网关和 UI renderer 继续以现有名称为边界。

## 替代方案

只依赖 typed deserialization，或在单一 schema 中允许所有字段并把错误留给运行时。前者无法向模型表达条件必填，后者会扩大错误调用面并增加重复的 UI/权限猜测，因此不采用。

## 影响

模型收到的工具契约更精确，错误在执行前暴露，批量调用更容易按操作做风险和并发判断。严格分支会拒绝旧的“带无关字段”调用；本版本允许该破坏性收紧，调用方应按当前 schema 重新生成参数。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --locked -p haven-tools`
- `cargo test --locked -p haven-tools operation_schemas_reject_cross_operation_arguments`
- `cargo test --locked -p haven-tools`（剪贴板 roundtrip 在无可用桌面剪贴板格式的环境下仍可能 flaky）

## 回滚 / 重置

这是内存中的工具 schema、运行时校验和结果字段变更，不修改数据库、配置或快照；回滚代码提交即可，不需要数据迁移或重置。
