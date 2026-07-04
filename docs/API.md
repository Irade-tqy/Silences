# DeepSeek v4 Flash Prefix Cache 校准数据

## 完整 Prompt 格式

```
[BOS(0)][User(128803)]content[AsstPrompt(128804)][EOS(1)]
```

- **BOS** = token ID 0 (`<｜begin▁of▁sentence｜>`)
- **User** = token ID 128803 (`<｜User｜>`)
- **AsstPrompt** = token ID 128804 (`<｜Assistant｜>`)
- **EOS** = token ID 1 (`<｜end▁of▁sentence｜>`)

### overhead（c = 4）

```
user(content="")     → [0, 128803, 128804, 1]            = 4 tok
user(content="hello") → [0, 128803, 33310, 128804, 1]     = 5 tok
system("sys") + user("hello") → [0, 44489, 128803, 33310, 128804, 1] = 6 tok
```

单条 user message 固定 overhead = **4 tok**（BOS + User + AsstPrompt + EOS）。

## 缓存分块规则

### b = 128 (chunk 大小)

缓存按 128 token 分块，从完整 prompt 的 token 0 开始对齐：

```
chunk 0: pos [0, 128)
chunk 1: pos [128, 256)
chunk 2: pos [256, 384)
...
```

### last chunk（生成边界）

最后一个 full chunk（紧接生成起始位置的 chunk）**不缓存**。需要额外 margin 才能落盘。
具体见「缓存读取阈值」。

## 缓存写入阈值

prompt 经过一次完整计算后，服务端写入 KV cache。再次发送相同 prompt 时可以命中。

### 精确匹配：每块需要 +1 tok margin

| 命中 chunks | 所需总 tok | 公式 |
|-------------|-----------|------|
| 1 chunk (128) | **total ≥ 129** | N×128 + 1 |
| 2 chunks (256) | **total ≥ 257** | N×128 + 1 |
| 3 chunks (384) | **total ≥ 385** | N×128 + 1 |

**规律：`total ≥ N×128 + 1` 时重复可命中 N 个 chunk。**

即：总 token 数必须 **超过** N×128，不能等于。最后 1 tok 起确认作用（服务端可能用它验证 prompt 在此 chunk 之后还有内容，确保 chunk 不是"悬空的"）。

### 实测数据

| content tok | 总 tok | R1 hit | R2 hit | chunks | 分析 |
|-------------|--------|--------|--------|--------|------|
| 110 | 114 | 0 | 0 | 0 | <129，无法命中 |
| 120 | 124 | 0 | 0 | 0 | <129 |
| **124** | **128** | 0 | **0** | 0 | **=128，正好边界，不命中！** |
| **125** | **129** | 0 | **128** | 1 | **+1 → 命中 1 chunk** |
| 126 | 130 | 0 | 128 | 1 | |
| 128 | 132 | 0 | 128 | 1 | （132≥129，1 chunk） |
| 252 | 256 | 0 | 128 | 1 | =256，正好边界，1 chunk |
| **253** | **257** | 0 | **256** | 2 | **+1 → 命中 2 chunks** |
| 256 | 260 | 0 | 256 | 2 | |
| 380 | 384 | 0 | 256 | 2 | =384，正好边界，2 chunks |
| **381** | **385** | 0 | **384** | 3 | **+1 → 命中 3 chunks** |
| 384 | 388 | 0 | 384 | 3 | |

### 总结

```
N chunks 命中: total ≥ N×128 + 1
```

反过来说：**如果 total 正好是 128 的倍数，最后一个 full chunk 不会被缓存。**

## 前缀继承（不同 prompt 共享前缀）

短 prompt 可以继承长 prompt 的缓存前缀，但需要额外 margin。

### 第一块继承阈值：a_继承 = total ≥ 135

| 先发（长） | 后发（短） | 结果 |
|-----------|-----------|------|
| 很长 | total=132 | 0 hit（<135） |
| 很长 | total=135 | 135 hit（继承 1 chunk） |

短 prompt 自身总 tok ≥ **135** 才能从长 prompt 继承 chunk 0 的缓存。

为什么是 135 不是 129？
- 精确匹配只需要 129（自己算过的缓存，直接从磁盘读）
- 继承匹配需要 135（额外 +6 tok 来确认另一个 prompt 的前缀确实匹配）

### 第二/三块继承阈值

继承后续 chunks 的阈值直接跟随精确匹配规则：

| 继承 chunks | 所需总 tok |
|-------------|-----------|
| 1 (128) | ≥ 135 |
| 2 (256) | ≥ 257 |
| 3 (384) | ≥ 385 |

即：`max(135, N×128 + 1)`

对于 N≥2：`N×128 + 1 ≥ 257 > 135`，所以后续块的阈值由 `+1` 规则决定。

### 等长前缀不共享

**同一长度的 prompt，即使前缀完全一致，也不共享缓存。**

验证：
```
R1: sys(140) + user(B_140)   → 计算并缓存 284 tok
R2: sys(140) + user(D_140)   → 0 hit（等长，不能完整匹配 R1 的缓存项）
R3: sys(140) + user(E_140)   → 0 hit
R4: sys(140) + user(F_140)   → 0 hit
R5: 重复 R1                   → 256 hit（精确匹配命中）
```

这意味着 agent loop 中 B_stable 固定 + B_delta 变化的模式，**每轮首次必然全量计算**。

### 短→长单向继承

**长 prompt 不能从短 prompt 继承缓存**（A+C 短 → A+B+C 长=0 hit），但**短 prompt 缓存了 chunk0 的话，长 prompt 可以复用 chunk0**。

关键实验对比：

| 前置缓存 | 后续请求 | 命中 | 原因 |
|---------|---------|------|------|
| A+C (257 tok) | A+B+C (385 tok) | **0** | 最大的前缀单元(256)在 chunk1 处不匹配，不降级检查更短单元 |
| A 单独 (130 tok) | A+B+C (385 tok) | **128** | A 只缓存了 chunk0，A+B+C 完全匹配这个 chunk0 前缀单元 |

结论：**服务端检查最大匹配前缀单元，不降级回退**。如果最大的缓存前缀单元不匹配，即使更短的 chunk0 完全一致，也不会命中。

实际影响：可以通过先发 B_stable（短）做 warmup，让后续 B_stable+B_delta（长）请求复用 chunk0。但需要额外一次 API 调用。

### 公共前缀检测落盘（不可用）

DeepSeek 官方文档描述了"公共前缀检测落盘"机制：系统检测到多次请求的公共前缀后，会将其作为独立缓存单元落盘。

实测结论：**该检测在分钟级时间窗内不触发**（等待 120s 后仍无命中）。推测为小时级批处理或需大量重复样本，对实时 agent loop 无帮助。

| 实验 | 结果 |
|------|------|
| A+B→A+D→A+E（间隔 2s） | 全部 0 hit |
| A+B→A+D→A+E（间隔 60s） | 全部 0 hit |
| A+B→A+D→A+E→A+F（间隔 120s） | 全部 0 hit |
| sys+B→sys+D→sys+E→sys+F（间隔 15s，x4） | 全部 0 hit |

## 缓存匹配规则

1. **精确匹配**：完全相同的 prompt → 整块命中（需 total ≥ N×128 + 1）
2. **前缀继承**：短 prompt 从长 prompt 继承前缀 chunks（需 total ≥ 135 启动继承）
3. **等长不共享**：相同长度的 prompt，即使前缀一致也不共享缓存（每轮首次必然全量计算）
4. **最后 partial chunk**：任何 <128 tok 的最后一个 chunk 永远不命中

## B_stable 优化

要最大化缓存命中，需让 B_stable（system prompt + 固定消息）对齐到 128 边界并留 margin：

```
B_stable_content_tokens + 4 ≥ 129  →  内容至少 125 tok 才能让 chunk 0 落盘
B_stable_content_tokens + 4 ≥ 257  →  内容至少 253 tok 才能让 chunk 0-1 落盘
```

由于 +1 规则，B_stable 的内容 tok 应为 `128×N - 4 + 1` = `128×N - 3`：

| N chunks | 内容 tok | 总 tok |
|----------|---------|--------|
| 1 | 125 | 129 |
| 2 | 253 | 257 |
| 3 | 381 | 385 |
| 4 | 509 | 513 |

B_delta 从 B_stable 结束后的第一个 full chunk 开始。

### 实际 Agent 场景

在 agent loop 中：
- **B_stable**（system prompt + 固定上下文）：对齐到 128×N - 3 tok
- **B_delta**（每轮变化的消息）：每轮首次必然全量计算（等长不共享规则）
- **同轮重试**：同一轮内重复完全相同 prompt → 满命中
- **公共前缀检测**：不可用（分钟级不触发）

## BPE 边界合并

独立构造的 token 串，拼接后 token 数减少：

```
A(128) + B(128) → A+B = 255 tok (非 256)
```

原因：A 末尾的字符与 B 开头的字符在 BPE 词表中存在更长的合并。

**解决方法**：在 token ID 层面操作，而非字符串层面。

## Token 层面构造内容（已验证）

构造 `a1 a2 a3 ...` 模式的 380 tok 字符串，各段按 token ID 切分：

```
A = IDs[0:126]   — 共享前缀
B = IDs[126:254] — 可变
C = IDs[254:380] — 共享后缀
D = B 的变体     — 内容不同
```

对于 `x1 x2 x3 ...` 模式，`x` 和 ` x` 各占一个 token（交替出现）：
- IDs[0] = 90  ("x")
- IDs[1] = 18  ("0")
- IDs[2] = 1527 (" x")
- IDs[3] = 19  ("1")
- ...交替...
- " x" → token 1527，改 " y" → token 383

Roundtrip 已验证稳定：
```python
tok.encode(tok.decode(ids)) == ids  # True (for stable patterns)
```
