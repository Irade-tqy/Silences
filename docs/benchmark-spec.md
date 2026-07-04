# Silences Score v1 — Benchmark 规范

> 版本 1.1 · 2026-07-01  
> 对象：Agentic Coding 框架成本测试  
> 核心指标：**API 实际 cost（人民币）**，唯一指标  

---

## 1. 基准假设

```
在 deepseek-v4-flash 模型下，使用 [框架名] 完成 
  (A) 修复 2 个真实 bug，和
  (B) 重实现一个被移除的完整功能
时，总 API cost ≤ Claude Code 完成同等任务的总 API cost。
```

---

## 2. 测试环境

| 项目 | 值 |
|------|-----|
| 模型 | `deepseek-v4-flash`（固定） |
| 测试项目 | `dailyPlanner` — Next.js 日历+番茄钟 Web 应用 |
| 软删除恢复 | 场景 B 的模板移除用 git stash 而非 clean clone，保留初始 commit 作为 diff 参照 |
| 初始状态 | 每个场景各一个独立 git worktree，`bench/<scenario>` 分支 |

---

## 3. 测试场景 A：Debug — 2 个真实 bug

### 3.1 Bug 1：浏览器休眠后倒计时滞后

**背景**：用户反馈「切了页面再切回来，系统时钟过去 2 分钟，番茄钟好像才过去 1 分钟」。排查发现计时器用 `setInterval` 每 1 秒递减 `timeLeft`，浏览器将后台页面 `setInterval` 节流，休眠期间不触发，恢复后从旧值继续。

**初始 prompt**（agent 收到的第一句话，固定）：
> 我用番茄钟，切换了一些页面后切回去，发现系统时钟过了 5 分钟，它才进行了 1 分钟。修一下。

**注入方式**：将 `PomodoroTimer.tsx` 的 `tick` 函数改为递减：

```diff
  const tick = useCallback(() => {
-   if (sessionStartRef.current === null) return;
-   const now = Date.now();
-   const elapsedMs = now - sessionStartRef.current - totalPauseMsRef.current;
-   const remaining = Math.max(0, totalTimeRef.current - Math.floor(elapsedMs / 1000));
-   setTimeLeft(remaining);
+   setTimeLeft(prev => Math.max(0, prev - 1));
  }, []);
```

最好的答案是把 timeLeft 完全删除。

**注意**：`sessionStart` 仍有记录（存于 state 及 ref），但 `tick` 不再引用它。这是真实会发生的「记录了 A，却用 B 计算」的 bug。

**Judge 提示词**（验证不通过时写 hint.txt）：
> 问题没修好。我切页面再回来，倒计时还是比实际时间慢。

### 3.2 Bug 2：专注结束到休息时 sessionStart 未重置

**背景**：用户说「我怎么写完总结休息直接没了？」，这是因为休息阶段在 `startTimer` 没有重置 `sessionStart`，`tick` 计算出的剩余时间为负数，显示时长为 0，一闪而过。

注意：不用 Bug 1 修复，就可以直接注入。伪装成休息开始的时候重算 timeLeft 用了没更新的 sessionStart。

**初始 prompt**（叠加）：
> 还有就是我不是让你写一个五分钟休息计时的功能吗？怎么一闪而过了？

**注入方式**：在 `startTimer` 中移除 `setSessionStart`：

```diff
  const startTimer = useCallback(
    (totalSec: number) => {
      clearTimer();
      setTotalTime(totalSec);
      setTimeLeft(totalSec);
-     setSessionStart(Date.now());
+     // BUG: sessionStart 未重置，休息阶段使用上一次专注的起始时间
      setTotalPauseMs(0);
      pauseStartRef.current = null;
      intervalRef.current = setInterval(tick, 1000);
    },
    [clearTimer, tick]
  );
```

**Judge 提示词**（验证不通过时写 hint.txt）：
> 不对，休息还是不对。跳了一下 00:00 就消失了。

---

## 4. 测试场景 B：Feature — 重实现模板功能

### 4.1 移除范围

切换到 `bench/feature-templates` worktree，该分支已移除：

**删除文件**：
- `app/templates/page.tsx`
- `components/templates/TemplateList.tsx`
- `components/templates/TemplateForm.tsx`

**清理代码**：
- `lib/types.ts` — 移除 `Template` 接口定义
- `lib/storage.ts` — 移除 `getDefaultTemplates()`、`addTemplate`、`updateTemplate`、`deleteTemplate`；`loadAll` 默认值中移除 templates
- `lib/useAppData.ts` — 移除 `addTemplate`、`updateTemplate`、`deleteTemplate` 回调；移除 `addEvent` 中模板联动逻辑
- `components/events/EventFormModal.tsx` — 移除模板选择 UI 全部代码及 `templates` prop
- `components/layout/HeaderViewToggle.tsx` — 移除 "模板" tab
- `app/page.tsx` — 移除 `template` 相关引用

### 4.2 Agent 任务

**初始 prompt**（叠加）：

> 另外帮我写一个模版功能。就是在创建事件的时候用模版添加，然后增加次数自动增加时间，名字自动变成「模版名 x 次」，备注使用和模版相同的，并且能追踪每个模版的创建个数。默认三个模版：练字（20min），阅读（30min），外出（45min）。用户可以自己添加。

### 4.3 验证检查项

| # | 检查项 | 判断方法 |
|---|--------|---------|
| V1 | 导航栏有「模板」tab，可访问 | 查看 `HeaderViewToggle.tsx` 的 TABS |
| V2 | 模板列表页存在，渲染不崩溃 | 组件存在且不报错 |
| V3 | 至少包含 3 个默认模板（练字/阅读/外出等） | 查看 `getDefaultTemplates()` 或等价初始化代码 |
| V4 | 新建模板：名称、耗时可填写并保存 | diff 中有新增 / 编辑流程 |
| V5 | 编辑模板：修改后可保存 | diff 中有编辑流程 |
| V6 | 删除模板：可删除并确认 | diff 中有删除 + confirm |
| V7 | 创建事件时可选模板 → 填写次数 → 自动填充字段 | diff 中 `EventFormModal.tsx` 有模板选择 UI |
| V8 | 从模板创建的事件关联 `templateId`，模板 `createdCount` 递增 | diff 中包含联动逻辑 |
| V9 | `npm run build` 无 TypeScript / 构建错误 | 直接运行 |

**反馈格式**（例如 V3 没有通过）：
> 模板页面打开后是空的，没有任何预置模板，没法直接选。

---

## 5. 验证协议（核心）

> 重要规则：**不用无头浏览器 / Playwright / 自动化行为验证**。  
> 原因：agent 生成的代码行为不可预测，自动化测试产生大量假阴性。

### 5.1 流程

```
                  ┌─────────────────────────────┐
                  │     被测框架（agent）         │
                  │  收到初始 prompt 或反馈提示    │
                  └──────────┬──────────────────┘
                             │ 生成：推理链 + 文件修改 (diff) + 输出
                             ▼
              ┌─────────────────────────────┐
              │       Judge 后端              │
              │  (另一个模型实例，可访问         │
              │   标准答案/验证清单)            │
              └──────────┬──────────────────┘
                         │ 判断：全部通过？
                    ┌────┴────┐
                    │         │
                  是/全部    否/部分
                    │         │
                    ▼         ▼
              记录 cost    写入简短提示词
              结束场景      (tool: 写提示词文件)
                               │
                               ▼
                        回到 agent，继续迭代
```

### 5.2 Judge 后端

- 独立进程/会话，与被测 agent 使用**同一模型**（deepseek-v4-flash）
- 拿到 agent 本次产生的：**推理链（reasoning）** + **文件修改 diff** + **最终输出**
- Judge 根据场景不同，持有一份**标准答案**（canonical fix diff）
- Judge 判定：diff 中的修改是否准确修复了 bug / 实现了功能
- Judge **只输出结论**，不向 agent 透露答案
- 如果未通过，Judge 调用一个 tool 写入**简短提示词**（1-2 句，描述现象，不指代码位置）

### 5.3 约定

- **禁止向 agent 透露答案内容**，包括代码位置、具体行号、函数名
- 提示词只能描述**用户视角的现象**（如「倒计时速度异常」「休息阶段剩余时间为 0」）
- Judge 不跟 agent 对话，只写提示词文件 → 框架读文件作为下一轮系统提示

### 5.4 标准答案（仅 Judge 持有）

**Bug 1 标准修复 diff**：
```diff
  const tick = useCallback(() => {
-   setTimeLeft(prev => Math.max(0, prev - 1));
+   if (sessionStartRef.current === null) return;
+   const now = Date.now();
+   const elapsedMs = now - sessionStartRef.current - totalPauseMsRef.current;
+   const remaining = Math.max(0, totalTimeRef.current - Math.floor(elapsedMs / 1000));
+   setTimeLeft(remaining);
  }, []);
```

**Bug 2 标准修复 diff**：
```diff
  const startTimer = useCallback(
    (totalSec: number) => {
      clearTimer();
      setTotalTime(totalSec);
      setTimeLeft(totalSec);
+     setSessionStart(Date.now());
      setTotalPauseMs(0);
      pauseStartRef.current = null;
      intervalRef.current = setInterval(tick, 1000);
    },
    [clearTimer, tick]
  );
```

---

## 6. 测试脚本设计

### 6.1 架构

```
benchmark/
  run.bat         # 入口：选择场景、启动框架、循环
  scenario-a.bat  # Debug 场景
  scenario-b.bat  # Feature 场景
  judge\          # Judge 后端
    judge.py      # 读取 agent 产出 → 对比标准答案 → 输出结果
    answers\      # 标准答案（canonical 修复 diff）
      bug1.diff
      bug2.diff
  records\        # 运行记录（每次运行的时间戳目录）
    2026-07-01T12-00-00\
      scenario-a\
        session.json       # 完整会话记录
        rounds\            # 每轮产出
          1\
            reasoning.md   # agent 推理链
            diff.patch     # 本次文件修改
            output.log     # agent 最终输出
            hint.txt       # Judge 反馈提示词（如有）
            cost.json      # 本轮的 API cost 明细
        summary.json       # 汇总
      scenario-b\
        ...
  results\        # 对比结果
    cc-baseline.json
    framework.json
```

### 6.2 记录格式

每轮 `cost.json`：
```json
{
  "round": 2,
  "timestamp": "2026-07-01T12:05:00Z",
  "framework": "claude-code",
  "model": "deepseek-v4-flash",
  "api_cost": 0.0042,
  "input_tokens": 18500,
  "output_tokens": 3200,
  "cache_hit_tokens": 9200,
  "wall_time_sec": 78
}
```

场景汇总 `summary.json`：
```json
{
  "scenario": "debug",
  "framework": "silences",
  "total_rounds": 3,
  "passed": true,
  "total_cost": 0.0128,
  "total_wall_time_sec": 245,
  "rounds": [
    {
      "round": 1,
      "cost": 0.0055,
      "passed": false,
      "hint": "番茄钟倒计时在页面切换后比实际慢，请检查计时计算方式"
    },
    {
      "round": 2,
      "cost": 0.0048,
      "passed": false,
      "hint": "休息阶段剩余时间显示异常，请检查阶段切换时的起始时间"
    },
    {
      "round": 3,
      "cost": 0.0025,
      "passed": true,
      "hint": null
    }
  ]
}
```

### 6.3 对照比较格式

```json
{
  "comparison": {
    "scenario": "debug",
    "model": "deepseek-v4-flash",
    "timestamp": "2026-07-01T14:00:00Z"
  },
  "frameworks": [
    {
      "name": "silences",
      "passed": true,
      "total_cost": 0.0128,
      "rounds": 3
    },
    {
      "name": "claude-code",
      "passed": true,
      "total_cost": 0.0150,
      "rounds": 2
    },
    {
      "name": "codewhale",
      "passed": true,
      "total_cost": 0.0450,
      "rounds": 4
    }
  ],
  "verdict": "silences vs claude-code: -14.7% (0.0128 vs 0.0150)"
}
```

---

## 7. Baseline 立即执行方案

### 7.1 准备工作

1. 为每个场景各创建一个 git worktree（基于 dailyPlanner）：
   ```
   dailyPlanner-bench-debug/     # bench/debug-pomodoro 分支，已注入 2 个 bug
   dailyPlanner-bench-feature/   # bench/feature-templates 分支，已删除模板
   ```
2. 编写 `judge/` 中的标准答案和判定逻辑
3. 编写 Judge 提示词（给 judge 模型的 system prompt）

### 7.2 运行 Baseline

```
# 场景 A：Debug
benchmark\run.bat --scenario debug --framework claude-code

# 场景 B：Feature
benchmark\run.bat --scenario feature --framework claude-code
```

### 7.3 验证目标

验证你的假设：**CodeWhale 完成同等任务消耗 > CC × 3**。

### 7.4 保存与恢复

所有运行记录保存在 `benchmark\records\`，格式化为 `YYYY-MM-DDTHH-mm-ss\` 目录。

```batch
REM 恢复某次运行记录
benchmark\replay.bat --record records\2026-07-01T12-00-00\scenario-a

REM 查看对比
benchmark\compare.bat --results results\
```

---

## 8. 公平性规则

| 规则 | 说明 |
|------|------|
| 同一模型 | 所有框架必须使用 `deepseek-v4-flash` |
| 同一 prompt | 初始 prompt 完全一致，仅格式适配 |
| 同一验证 | Judge 判断标准统一，不因框架不同而放宽 |
| 同一代码 | 各框架看到的同一个 git worktree，状态完全一致 |
| 不透露答案 | 提示词只描述现象，不暗示修复方式 |
| 缓存清空 | 每个框架跑之前 clean worktree，排除缓存干扰 |

---

## 9. 后续扩展

- v1.2: 批量运行（每个场景 3 次取中位数）
- v1.3: 多模型对比
