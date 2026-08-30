# ADR 0027：LLM 适配器 web search 边界

## 背景

不同 provider 的内置 web search 事件格式不同，但 adapter、Agent 和 UI 共享
三类稳定语义：echo 前的 skeleton 规范化、同一 call 的富 payload 去重，以及
将 citations/result 转成统一的 `{queries, results}`。这些逻辑此前与其它
adapter helper 一起位于 `adapters/mod.rs`，协议细节和跨层展示契约耦合在同一
热点文件中。

## 决定

- 新增内部 `adapters/web_search.rs`，唯一拥有 web-search call 的规范化、citation
  提取、结果 DTO 组装和按 call id 的富 payload 去重。
- provider adapter 继续捕获各自的 SSE/event payload；Agent 继续决定如何执行
  web-search 结果；UI 继续消费 `web_search_result_of` 的统一结果形状。
- 保持缺失/异常 `action` 的回填、Anthropic `result.query`、xAI URL citations、
  240 字符 snippet 上限和 rich-over-skeleton 排序行为不变。
- `web_search_result_of` 保持 `haven_llm` 的公共 re-export；其余 helper 仍为
  `haven-llm` crate 内部 API。

## 替代方案

- 每个 provider adapter 各自规范化和去重：会让 Agent/UI 收到不同结果形状，拒绝。
- 让 Agent 直接解析 provider wire payload：跨越 LLM 边界并泄漏 provider 细节，拒绝。
- 建立跨 crate web-search trait：当前稳定契约是 JSON DTO，trait 会扩大 API 和
  测试替身面，暂不采用。

## 影响

这是 `haven-llm` 内部模块重组，保留 `haven_llm::web_search_result_of` 的公共
入口。provider wire、Agent 投影、UI 展示、配置格式和持久化数据均不变，不需要
配置、数据库或缓存重置。

## 验证

```text
cargo fmt --all -- --check
cargo check --locked -p haven-llm
cargo test --locked -p haven-llm --lib -- --test-threads=1
cargo clippy --locked -p haven-llm --lib -- -D warnings
```

重点回归跨 provider citation 提取、action 回填、空结果、同 id 去重及公共
re-export。

## 回滚与重置

代码回滚时删除 `adapters/web_search.rs` 和模块登记，把其函数与测试恢复到
`adapters/mod.rs`；本次不改变 schema、序列化、配置或持久化数据，不需要用户
重置。
