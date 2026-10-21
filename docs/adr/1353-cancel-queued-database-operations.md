# ADR 1353：阻止已取消的排队数据库操作迟到写入

## 状态

已实现。

## 背景

`run_blocking_cancellable` 原先只中断取消时已登记的 SQLite 连接。操作在 blocking executor 或连接池排队时没有连接可中断；两秒等待超时后调用方收到错误，worker 却仍可能取得连接并提交 checkpoint。另一个窗口是连接已取得但尚未开始 SQL，SQLite 的单次 interrupt 不会阻止之后的语句。

## 决定

- worker-local 取消上下文同时持有 token 与 interrupt registry，以 RAII 恢复上下文。提交 worker 前与实际执行闭包前均检查取消。
- 连接池在每轮取得连接前、opener 返回及 interrupt 登记后检查取消。可取消的连接等待以 25ms 为间隔检查 token；普通调用保留原等待策略。取消通过 `try_conn` 的错误传播，生产 repository 不再使用会 panic 的 fixture accessor。
- 在可取消操作持有的连接上启用 rusqlite 已有的 `hooks` feature，每条 SQLite VM 指令通过 progress handler 检查 token，覆盖 BEGIN、缓存语句、后续写入与 COMMIT；仍保留 interrupt 与 100ms busy timeout 处理正在执行的调用。
- 归还连接前先移除 progress handler，再防御性回滚并恢复 busy timeout，保证未提交事务被清理，连接复用不继承取消状态。fact write gate 取得后也检查取消。
- Agent 最终退出的边界完整性读取不复用已取消的工作 token；已执行或已中断 ToolCall 的结果提交使用独立 cleanup token，并用 drop guard 在收尾 future 被丢弃时取消其 SQL。这些是显式收尾操作，普通 transcript 写入仍受 turn token 约束。
- 两秒 bounded join 仍保留为异常扩展或非 SQL 阻塞的兜底；detached worker 后续取得连接和执行 SQL 仍受 token 约束。取消不撤销已经完成的提交，不为任意闭包的非数据库副作用提供取消保证。

## 替代方案

仅多发一次 interrupt 仍有无活动语句及后续 checkout 的窗口；仅在调用方返回取消错误会留下活跃 worker；仅在 checkout 后检查无法覆盖已取得连接后才取消的 SQL。因此组合 token 检查与 SQLite progress handler。

## 影响与回滚

改动涉及 Memory 内部连接获取、取消执行语义及 Agent 取消收尾，公开 typed store 签名、schema、事件与持久数据格式不变，无需重置数据库。repository 的机械变更用于正常传播 checkout 取消错误。progress handler 仅在可取消连接持有期间启用，逐指令检查增加执行开销，以短语句的取消正确性优先。

`db.rs` 保留连接池和取消执行这一职责范围；新增内容主要是同文件回归测试，暂不为行数拆分模块。回滚本提交会恢复迟到写入风险。

## 验证

- 隔离内存数据库：耗尽连接、取消、等待调用方返回、释放连接、等待 worker 结束，确认 checkpoint 缺失并可继续正常写入。修复前该测试稳定得到迟到的 `Some(7)`。
- 单 blocking worker 队列取消不执行闭包；opener 期间取消归还容量；连接取得后在 BEGIN 前、缓存写入前、COMMIT 前取消均失败且无提交，回滚后连接可复用。
- 保留 SQLite 锁等待中断测试；运行 Rust workspace 格式、locked 编译、严格 Clippy 与测试，以及 ADR 索引检查。
- Agent 测试覆盖取消后最终边界、继续会话、工具 batch 中断与 rollback，以及 deadline 后结果修复；这些测试防止把必要的收尾误当成已取消的业务工作。
