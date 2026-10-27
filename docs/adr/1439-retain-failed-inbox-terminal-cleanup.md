# ADR 1439：Inbox 注销失败保留终态清理责任

## 状态

已实施 — 2026-10-27。

## 背景与证据

R04 后续复核确认，`status.rs::unregister_from_inbox` 吞掉 registry 读取、注销、标记离线与 blocking join 错误。`finish_ended_session` 随后成功，终态清理 guard 完成并移除 Actor；无后代或 `cascade=false` 时没有后代枚举失败替此错误保留责任。root registry 条目可能残留，child 在线状态最多延续到原 heartbeat 后 300 秒。显式再次 End 可以补做；原自动清理责任已经丢失。

## 决定与不变量

- Inbox 注销返回明确结果；失败记录净化日志，上层错误只保留稳定描述。
- Inbox 失败不跳过 overlay、scheduled confirmation 与 session trust 清理，也不阻止既有级联尝试；所有必需清理成功后才清除后代提示并完成 guard。
- 驻留 Actor 的 Completed 已提交后继续返回成功；清理失败由原 guard 保留 Actor 与原 retry policy，交给既有 worker 自动重试。
- 无 Actor 的显式 End 沿用现有 post-commit cleanup 错误传播：可能返回错误但数据库已 Completed；不回滚、不发送 EndPaused，再次 End 可重试，已提交的 SessionEnd hook 不重复执行。
- 注销与标记离线沿用 Messaging 的幂等操作。child 保留父关系与元数据，只改变在线状态。
- 不新增清理 owner、持久标记或 schema，不改变单 worker、batch、退避、取消和 shutdown final drain；无 Actor 自动恢复、backlog 总量与跨重启补做仍不在本切片承诺内。

## 替代方案与范围

仅记录失败会继续丢失自动重试责任；遇到 inbox 错误立即退出会保留 session trust 并跳过独立清理；回滚 Completed 会混淆持久提交与后续清理。复用原 guard 能保持稳定 Actor identity、cascade 策略与既有生命周期仲裁。

`status.rs` 已触发职责复核，本切片保持原终态 owner，新故障矩阵放入私有测试模块；不扩大生产 facade，不另建通用任务框架。

## 影响、重置与回滚

仅 Agent 私有清理失败行为变化，无 IPC shape、事件、配置或数据库格式变化，无需数据重置。回滚清理错误传播及测试会恢复注销暂时失败后的责任丢失风险。

## 验证

- 旧实现定向 4 项中 3 项先失败：root/child 错误移除 Actor，以及 actorless 清理失败被当作成功；信任撤销/false 原行为通过。
- 修复后 5 项故障测试通过，覆盖 root/child 修复后自动清理、父关系保留、失败 final drain、普通 worker 退避恢复、显式 false、信任撤销与 actorless post-commit 错误/显式重试；同时断言不发布错误的 EndPaused。重复 SessionEnd hook 由源码中的已提交状态检查保持，本轮未新增其专门断言。
- Rust 1.99.0；Agent 全量 710 passed、0 failed、1 ignored，另 2 项手动性能 profile ignored，doc tests 通过。
- cargo check --workspace --locked、cargo clippy --workspace --all-targets --locked -- -D warnings 和 cargo fmt --all -- --check 通过。
- cargo test --workspace --locked -- --test-threads=4 全部通过，Tools 1014 passed、16 ignored，doc tests 通过；现有忽略项保持不变。
- ADR index（1336 条）、命名/调用点复核及 staged diff 检查通过。真实 Windows 发布 Gate 仍开放，未生成发布包。
