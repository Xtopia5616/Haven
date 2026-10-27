# ADR 1436：后代结束失败保留终态清理工作

## 状态

已实施 — 2026-10-27。

## 背景与准入证据

R04 复核发现，`cascade_end_children` 忽略后代枚举与子会话 End 失败，`finish_ended_session` 随后清除 Actor 的后代提示，`finish_terminal_cleanup` 仍标记清理完成并移除父 Actor。子会话因暂时存储故障保留 Paused 时，父会话的后续 End 因无 Actor 而跳过后代扫描，原工作不再有重试 owner。

本切片只修复失败被当作清理完成的路径，不改变父会话 Completed 的持久提交条件，不扩展整个清理 backlog 的容量或跨重启恢复范围。

## 决定与不变量

- 后代枚举或任一后代 End 失败时，清理返回未完成；独立后代继续得到结束尝试，失败日志经过现有错误净化。
- 父会话已提交的 Completed 不回滚。原 terminal cleanup guard 保留父 Actor、后代提示及 cascade/remove-error 重试策略，并将工作交还既有队列；全部成功后才清除后代提示、解除清理责任并按身份移除 Actor。
- 不把父会话已成功的持久状态提交伪报为提交失败。迟到的 run-exit 与显式重复 End 继续通过已有 lifecycle gate 和 Actor identity 仲裁。
- Session trust 在级联前撤销，不因子会话失败而继续保留父会话授权。
- 普通失败与超时均使用既有单 worker、FIFO、32 项 batch、5 秒期限和 1 秒退避；取消安全 lease 与 shutdown 单轮 final drain 保持不变。
- `cascade=false` 保持不结束后代；actorless End 继续不扫描历史后代。后代提示和 retry policy 仍为进程内状态，不新增 schema、持久任务或跨重启补做保证。

## 替代方案

只记录错误会继续丢失工作；清理失败后回退父会话状态会混淆已提交事实；新建后台级联任务会重复清理 owner。直接向上抛出子会话错误也不能表达父会话状态已经提交的事实。保留原 guard 和工作队列即可复用取消、重试与退出边界。

## 影响、重置与回滚

仅 Agent 私有生命周期行为变化，无公共 API、IPC、事件格式、数据库或配置变更，无需重置。回滚涉及 status 清理结果、worker 失败退避与回归测试；回滚会恢复暂时失败后丢失后代结束工作的风险。`status.rs` 已达到职责复核阈值，本切片保留其现有领域 owner，调度仍在私有 `terminal_cleanup.rs`；新增故障矩阵独立放在测试模块，不扩大生产 facade。

## 验证

- 旧实现先行故障注入：子会话 Completed 更新被 SQLite trigger 拒绝；移除 trigger 后第二次父 End 仍留下 Paused 子会话。两次 End 均为 Ok(Completed)，父 Actor 已移除，队列虽有一项但 actorless retry 无法继续。
- 修复后定向 8 项测试通过：重复 End、worker 自动恢复、兄弟部分成功、显式 false 策略、普通失败退避、final drain 持续失败、取消后下一 worker 恢复，以及隔离 agents.json 损坏后的恢复。
- cargo check --workspace --locked、cargo clippy --workspace --locked -- -D warnings 及 Agent --all-targets 含依赖严格 Clippy 通过。
- 额外 workspace --all-targets 严格 Clippy 暴露未修改的 Common/Messaging/Memory/LLM 测试代码共 10 项既有告警；本切片不将该额外检查记为通过，后续按独立维护切片处理。
- Rust 1.99.0；cargo test --workspace --locked -- --test-threads=4 全部通过，Agent 705 passed、1 ignored，另 2 项手动性能 profile 保持 ignored；Tools 1014 passed、16 ignored，doc tests 通过。
- cargo fmt --all -- --check、ADR index（1333 条）及 diff 检查通过。真实 Windows 发布 Gate 仍保持开放，未生成发布包。
