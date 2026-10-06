# ADR 0539：区分路由配置与请求执行策略

## 状态

已采纳并实施；`haven-llm` crate 门禁通过。

## 背景

`haven_common::config::RequestPolicy` 是持久化路由配置的一项：它将逻辑 `RequestKind` 映射到 primary model，归属配置模型，并参与 `RouterConfig` 的序列化。`haven-llm::request_pipeline::RequestPolicy` 则是 crate 私有的运行时快照，捕获某次请求使用的 retry 参数与 total timeout，供 complete、embedding 和 streaming 执行路径共享。两者没有相同的数据或生命周期，只是名字相同，搜索与调用代码容易让人误以为它们是同一策略。

## 决定

1. 将 LLM 私有运行时类型改名为 `RequestExecutionPolicy`；Common 配置类型继续叫 `RequestPolicy`。
2. 保持 retry/timeout 快照的构造时机、字段、路由选择、配置序列化与请求行为不变。
3. 在架构说明和重构路线图中记录二者各自的 owner 与职责，避免把不同层的策略对象合并。

## 替代方案

- 合并成一个跨 crate 策略类型：拒绝。持久化的模型路由选择与一次在途请求的执行预算具有不同 owner、数据和生命周期；合并会让配置层承载执行态，或让运行时对象暴露持久化配置职责。
- 保留两个同名类型，仅依靠模块路径区分：拒绝。模块路径虽可消歧编译引用，但类型名称没有表达用途，跨文件检索与阅读仍含糊。
- 重命名 Common 配置类型或配置字段：拒绝。现有 `RequestPolicy` 已准确表示路由配置契约，变更还会扩大配置/API 面。

## 影响与验证

- 仅重命名 `haven-llm` crate 内的私有类型及其调用点和架构描述。Common 配置类型、配置 JSON、持久化数据、路由选择与运行行为均不变，无需重置或迁移。
- 验证通过：`cargo fmt --all -- --check`、`cargo check --locked -p haven-llm`、`cargo clippy --locked -p haven-llm -- -D warnings`、`cargo test --locked -p haven-llm`（508 passed、1 ignored）、`scripts/check-adr-index.ps1`（522 条唯一编号记录、链接解析通过）与 `git diff --check`。

## 回滚

将 crate 私有类型与引用恢复为 `RequestPolicy`，并同步恢复架构说明即可；没有数据、配置或 wire contract 回滚。
