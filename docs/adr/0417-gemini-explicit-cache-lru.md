# ADR 0417：Gemini explicit cache 使用有界 LRU

- 状态：Accepted
- 日期：2026-09-30
- 范围：`haven-llm` Gemini `cachedContents` runtime cache

## 背景

ADR 0193 为每个 Gemini adapter 保存一个 explicit cache fingerprint。会话/MEMORY
内容属于当前 `systemInstruction`，切换会话或刷新 MEMORY 会生成新 fingerprint；从
会话 A 切换到 B 再回到 A 时，即使 A 的 provider resource 仍有效，本地也已忘记其
resource name，导致重复创建。

## 决定

1. 每个 Gemini adapter 按现有 fingerprint 保留最多 4 个 cache 状态，按最近使用顺序
   排列。fingerprint 仍由模型、完整 `systemInstruction` 和工具 projection 组成；不把
   session ID 加入 key，也不改变 session/MEMORY 在 prompt 中的角色或顺序。
2. 命中 ready entry 时将其移到 LRU 首位。缓存创建成功后按同一 fingerprint 写入
   resource name 与本地到期时间；entry 到期后移除并允许重新创建。
3. 缓存创建失败和 provider 拒绝缓存 resource 的 5 分钟负缓存按 fingerprint 保存；
   provider 拒绝只让匹配 resource name 的 entry 进入负缓存，不清除其它会话的 entry。
4. 超出 4 项时淘汰 LRU entry。淘汰只删除本地引用；provider resource 仍由创建时的
   1 小时 TTL 清理，不额外发送远端 delete 请求。

## 影响与验证

多会话交替时，只要 fingerprint 未变且 entry 未过期，就能复用此前的 provider
resource。会话描述/MEMORY 变化仍创建不同缓存，保持当前 Gemini prompt 语义。最多
4 个缓存状态（包括负缓存）占用适配器本地内存；被淘汰的远端资源可能在 TTL 到期前
继续计费。缓存 identity 与 prompt 内容不进入持久化诊断。

回归覆盖 A/B/C/A/B fingerprint 交替复用、LRU 容量与 recency、过期重建、负缓存隔离
和 resource-specific invalidation。

验证命令：

```text
cargo fmt --all -- --check
cargo test --locked -p haven-llm --lib adapters::gemini
cargo clippy --locked -p haven-llm --lib -- -D warnings
```

## 回滚

回退本 ADR 对应代码与测试即可。没有数据库、配置、wire 或用户数据重置要求。
