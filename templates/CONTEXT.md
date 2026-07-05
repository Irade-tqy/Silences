# CONTEXT.md

> 只保留对后续步骤真正重要的信息，严禁流水账。
> 以下是一个示例，看过之后即可重写

## 未完成任务

<!-- 待处理的任务列表 -->
- [ ] 修复 dailyPlanner 时区问题

## 关键信息

<!-- 按需分区记录，每一条都是可验证的事实或精确定位。-->

### 代码位置

- 时间计算核心：`src/utils/dateHelper.ts:42-78`  
  函数 `getEndOfDay(timestamp, timezone)` 未处理传入的 `timezone` 参数，硬编码使用 `'UTC'`

### 接口/数据格式

- 前端调用 `/api/planner/daily` 的请求格式（`src/api/planner.ts:15`）：
  ```json
  { "date": "2026-07-05", "timezone": "Asia/Shanghai" }
  ```
- 后端对应路由 `POST /planner/daily` 在 `server/routes/planner.go:88`

### 已验证事实

- 单元测试 `dateHelper.test.ts:130` 对该函数的测试用例只覆盖了 UTC，缺少其他时区

### 上下文约束

- 修改必须向后兼容，已存在 3 个调用点使用默认 UTC 行为
- 前端错误提示文案在 `src/i18n/zh-CN.json:218`，修复后可能需要调整

## 注意事项

- 不要改动 `src/utils/dateHelper.ts` 中其他日期函数，避免引入回归
- 测试文件中对 UTC 的预期可能是早期设计，需与产品确认是否保留默认 UTC 行为

### 依赖/工具

- 用 `npm run test:unit -- --grep dateHelper` 运行相关测试
- 项目使用 `dayjs` + `timezone` 插件处理时区

---

**关键信息怎么写**

禁止：流水账式行动总结、总结用户消息

错误写法（按行动顺序组织、总结）

```
- 探索了 dateHelper.ts，其中第 42-78 行为函数 getEndOfDay
- 查了调用点，为：xxx
- 检查了测试文件里有没有覆盖时区
```

正确写法（精确、按信息类型组织）

```
- 缺陷位置：`src/utils/dateHelper.ts:42-78`，`getEndOfDay` 忽略时区参数
- 影响范围：3 处调用（`src/services/planner.ts:102`, `src/components/DailyView.tsx:56`, `src/api/report.ts:34`）
- 测试缺口：`dateHelper.test.ts:130` 仅测试 UTC，需增加 `Asia/Shanghai` 用例
- 接口证据：前端 payload `{timezone:"Asia/Shanghai"}` 正确传递，后端未使用
```