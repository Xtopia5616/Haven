# ADR 1370：Agent 模型消费者共享运行时发布点

## 状态

已接受并实施 — 2026-10-23。对应分领域审查 R01。

## 背景

AgentLayer 原先只替换 ReActEngine 的 router，标题、记忆推断、prompt embedding 与索引仍持有构造时的 Arc。设置应用后新请求可能继续使用旧模型或凭据。

## 决定

Agent 内部用私有 ModelRuntime 保存唯一可替换 router 与单调代次。组合根将同一 handle 注入 ReAct、标题、记忆推断及已有 MemoryService/embedding index；不向跨 crate 消费者暴露可变注册表。独立构造且未提供 router 的 MemoryService 继续只做关键词检索。

每次逻辑请求取得 router 快照，释放锁后才 await。标题的配置检查与调用、prompt 的向量空间身份与 embedding、索引维护的一整次有界 pass、向量 recall 的身份与调用使用同一快照。切换之后的新请求取新 router；在途请求可用旧快照完成，不宣称撤销已经发送的请求。

prompt-memory 缓存加入模型发布代次，即使 vector_space_id 未变化，切换凭据后也重新检索。持久向量仍使用 LLM owner 的 vector_space_id，空间变化继续使用已有关键词降级与有界维护重建；不混查旧空间，不新增同步重建或 worker。

## 替代方案与边界

逐一调用多个 setter 会继续要求手工同步多个真源，故不采用。此切片不解决 App router/媒体客户端的组合发布与部分失败（R03），也不增加配置 reapply（R02）。不改权限或执行上下文。

新增发布职责独立放在私有 `model_runtime.rs`。现有超过 800 行的 `layer.rs` 与 `react/mod.rs` 只改组合接线和既有路由读取，不引入新业务 owner；`memory_worker.rs` 仅增加测试用推断操作入口，因此本切片不顺带拆分这些模块。

## 验证

A/B fake clients 覆盖热切换后的聊天路由、既有标题实例、实际 worker 推断、prompt 缓存、查询及写入 embedding，并验证旧在途标题可完成。另验证无 FastChat/embedding 路由时降级、不回退到旧 router，以及向量空间切换时隔离。

适用门禁：Rust 格式、Agent locked 编译、严格 Clippy 与完整 Agent 测试；因不改变跨 crate API、持久化格式或 Rust/UI wire，无需 UI 构建或 IPC 再生成。

Windows 验证：定向 `cargo test -p haven-agent --locked router_reload` 两项通过；完整 `cargo test -p haven-agent --locked` 为 679 passed、1 ignored，另有两项手动性能测试 ignored，doctest 通过（0 项）。`cargo check -p haven-agent --locked`、`cargo clippy -p haven-agent --locked -- -D warnings`、格式检查、ADR 索引/本地链接及本切片 diff 检查均通过。

初次在默认 target 执行编译和定向测试因 D 盘空间耗尽（os error 112）失败。上述通过结果使用 `C:\Users\olive\.cache\haven-r01-target`，`CARGO_PROFILE_DEV_DEBUG=0`、`CARGO_PROFILE_TEST_DEBUG=0`、`CARGO_INCREMENTAL=0`；仅调整构建产物与符号，不改变功能配置。D 盘空间恢复后才进行精确暂存与提交。

## 数据、回滚与风险

无 schema、配置、事件或 IPC 格式变化，无需重置数据。缓存是最多 32 项的进程内派生状态；代次不持久化。回滚代码即可，既有向量空间标记不变。运行时快照仅替换 router，不保证每种 provider 的凭据撤销能取消旧在途请求。
