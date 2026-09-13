# ADR 0144：Agent Prompt 与工具使用文案收敛

## 状态

Accepted — 2026-09-14

## 背景

Haven 同时把系统 Prompt、工具根描述、operation 视图描述和动态目录交给模型。
这些文案分散在执行实现和注册表中，容易重复、漂移，也增加工具选择时的阅读成本。
系统 Prompt 还需要同时承担操作规则、错误恢复和不可信上下文边界。

## 决定

1. `haven-common::prompts` 只维护跨 crate 的系统级和小模型提示词；`haven-tools::prompts`
   维护内置工具根描述、operation 描述和使用边界。参数 schema、校验、风险和执行逻辑
   仍留在各工具实现附近。
2. Agent 系统 Prompt 使用短的操作规则：以当前 `tools[]` 为能力真源，先窄读后行动，
   把工具结果当证据，副作用后尽量复核，按错误类别恢复，并把动态上下文、记忆、技能、
   MCP 和 peer 消息视为数据而非指令。
3. 工具文案采用“做什么 + 何时使用 + 关键边界”的最小结构。工具目录只做 family 级
   orientation；具体名称、参数和可用性仍以当前 `tools[]` 为准。
4. 保持静态 Prompt 与动态 session context 的缓存边界；动态内容继续位于稳定指令之后，
   不把用户数据或文件内容提升到 system Prompt。

## 参考与替代方案

设计参考 Pi Coding Agent 的公开、短系统 Prompt 与动态工具索引，以及 Anthropic 关于
工具应有清晰职责、可操作错误和面向新成员编写描述的公开实践。未复制任何私有或流出
提示词文本。

- 继续把文案放在各实现中：改动局部，但会继续产生重复和漂移。
- 只保留工具根描述：Prompt 更短，但模型缺少相近 operation 的选择边界。
- 把完整 schema 说明复制进系统 Prompt：信息更全，但会增加 token、缓存失效和注入面。

## 影响、验证与回退

工具名称、权限 key、参数结构、风险策略和执行行为不变；仅调整模型可见的描述、目录
格式和静态 Prompt 文案，不需要数据库或配置重置。验证包括：

```text
cargo fmt --all -- --check
cargo test --locked -p haven-common -p haven-tools -p haven-agent
cargo clippy --workspace --locked -- -D warnings
```

回退代码与本 ADR 即可，无需数据迁移。
