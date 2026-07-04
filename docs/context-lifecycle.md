# 上下文生命周期（Context Lifecycle）

> 从节点 0 到最后一个 LLM 调用的全链路上下文结构。

---

## 请求 1：用户首次下发任务

### Node 1 — 首轮 LLM 调用

```
系统提示词
SILENCES.md(user, name=绝对路径)
用户消息(user, name=user)：「创建一个简单的 task……」
                 ↑ 在这里创建 DeepSeek 前缀缓存
```

→ 模型决定加任务 → 调用 `add_task` → `start_task` → `end_task`

---

### Node 2 — end_task → defer_rollback → u_orch 注入后，下一轮 LLM

```
系统提示词
SILENCES.md
用户消息
  ← 以下是上一轮产生的全部工具细节（未回滚）→
assistant(tool_calls=[add_task])
tool_result: [添加任务] 复述需求
assistant(tool_calls=[start_task])
tool_result: [开始任务] 复述需求
assistant(tool_calls=[end_task])
tool_result: [完成任务] 复述需求
orch(user, name=orch)：「任务 xxx 已完成。只更新 CONTEXT.md，记录完成进度……」
```

→ 模型写 CONTEXT.md（write tool）→ 输出总结 → 无更多 tool call → **进入 pending_rollback**

> 每一轮 tool loop 都共享前缀缓存。

---

### Node 2.5 — Warmup（rollback 块内，摘要写入后、CONTEXT.md 刷新前）

```
系统提示词
SILENCES.md
用户消息 （吃缓存）
摘要(assistant)          ← 本轮总结，checkpoint 已前移保护（缓存到这里）
```

→ 调用 `warmup_prefix(&messages[..checkpoint])`，发 `max_tokens=1` → **等 1s** 让缓存稳定。

---

### Node 3 — 第一次 rollback 后，下一轮 LLM

```
系统提示词
SILENCES.md
用户消息
摘要(assistant)          ← 本轮总结，checkpoint 已前移保护（吃缓存）
CONTEXT.md(user, name=绝对路径)  ← 唯一一份（删旧插新）
任务列表(user, name=task_list.md) ← 系统维护，已完成 + 待处理
orch(user, name=orch)：「继续执行后续任务」
```

→ 工具细节全部被砍。模型通过任务列表看到"还有 ipconfig 等任务"，继续执行。

---

### Node 3.5 — Warmup again

```
系统提示词
SILENCES.md
用户消息
摘要1(assistant)   ← 第一次 rollback 保留下来的（吃缓存）
摘要2(assistant)   ← 第二次 rollback 新推的（建）
```

---

### Node 4 — 第二轮（ipconfig）同样的流程，最终队列为空

```
系统提示词
SILENCES.md
用户消息
摘要1(assistant)   ← 第一次 rollback 保留下来的
摘要2(assistant)   ← 第二次 rollback 新推的（吃缓存）
CONTEXT.md(user)   ← 最新唯一一份（旧版已删）
任务列表(user)     ← 系统维护「已完成：复述；待处理：ipconfig」
orch(user, name=orch)：「所有任务已完成。请生成一份全面的最终总结，然后结束。」
```

---

### Node 5 — 最终总结输出

```
系统提示词
SILENCES.md
用户消息
摘要1
摘要2（吃缓存）
CONTEXT.md
任务列表（全部已完成，队列已空）
orch: 最终总结
→ assistant:「## 最终总结\n\n## 环境就绪\n...」
```

→ 模型写总结的完整过程被 `save_message` 到 DB，agent 结束。

---

## 请求 2：用户发新消息

### Node 6 — handle_chat 加载 + 首轮 LLM 调用

```
系统提示词
SILENCES.md(user, name=绝对路径)   ← 从磁盘加载
  ← 以下来自 DB get_messages（非 hidden）：
用户消息(name=user)：「创建一个简单的 task……」
摘要1(assistant, 纯文字)
摘要2(assistant, 纯文字)
CONTEXT.md(user)     ← 唯一一份
任务列表(user)       ← 系统维护
orch: 最终总结
ass 写总结完整过程（比如总结前可能还要调用一下 tool）
最终总结(assistant)（吃缓存）
用户消息(name=user)：「你现在的上下文里有什么」（建缓存）
```

> `orch` 和 `ass` 之间的消息始终保留（不被隐藏）。新用户消息建新缓存。

---

## 关键规则

| 规则 | 说明 |
|------|------|
| **Warmup 只在 agent 循环内** | `handle_chat` 不预热。Node 1 第一次 LLM 调用自然建缓存。 |
| **Warmup 时机** | rollback 块内，checkpoint 前移后、CONTEXT.md 刷新前。 |
| **Warmup 内容** | `messages[..checkpoint]` = `[system, SILENCES.md, user, ...全部已保留摘要]`。 |
| **Warmup 时序** | 发 `max_tokens=1` 请求 → **等 1s**（先发后等）。 |
| **保留边界** | `last_preserved_id` 每次 rollback 后推进，使摘要 + CONTEXT.md + 任务列表不被下次 rollback 隐藏。 |
| **摘要持久化** | rollback 时 `save_message` 到 DB，下次 `get_messages` 能正常加载。 |
| **CONTEXT.md 唯一** | 每次 rollback 从 DB 删掉该会话所有旧 CONTEXT.md 条目，再写入最新版。DB 中只有一条。 |
| **任务列表** | 系统自动维护，`queue.format_for_context()` 输出 markdown（已完成 + 待处理）。紧贴 CONTEXT.md，删旧写新。 |
| **外部不注入 CONTEXT.md** | `handle_chat` 只注入 SILENCES.md，CONTEXT.md + task_list 完全由 agent 内部 rollback 管理。 |
| **最终总结** | Node 5 的输出保存到 DB，Node 6 能从 `get_messages` 正常加载并吃缓存。 |
