# 绑定审阅复用作者目录与同 Task 复审

## 目标与边界

- Tend 独立审阅进入作者所在 Runner 的本地队列，沿作者的实际 `workspace_key`、父 Issue 或 Task fallback 使用同一目录。只有现有 Run 停妥证据允许另一方进入。PR/CI 等待的独占不变，普通目录竞争者不能插入绑定审阅的交接。
- 审阅准备只核对原目录、原 Git 挂载及本机原件。Git 按完整 SHA 从对象库读取，不切换 HEAD，不改作者已跟踪文件。文件从作者 Task 的 Runner 交付副本读取。
- `tend_request_review` 可带原 `review_task_id` 显式提交新候选。复用原 Issue、Task、Reviewer 和正常可恢复的原生 thread，并独立保存每轮请求、范围、证据与结果身份。保留正常的 Owner 结案、取消和 Issue 树归属。未知执行或普通断链不能触发新 session。
- 维持 Runner 协议 v2 及原原生 subagent 记录。用可选 v2 能力事实避免旧 Runner 把共享审阅当成隔离 worktree 新建。沿已受理身份对账，不部署、不操作生产 Issue。

## 实施顺序

1. 统一 Runner 的目录身份与排队准入，在真实停妥事务归还目录和 slot。保留 `waiting_external` 的 `retains_workspace`。仅在作者原目录验证通过后启动审阅。
2. 将结构化结论、无结论停妥和重试绑定到单轮原请求。以持久输入重新调度原 Review Task，不改原分配合同。复用现有原生接续与历史恢复，区分确认不可恢复和暂时未知。
3. 更新角色、工具及生命周期契约，审阅实际候选后提交通往 `origin/main`。

## 验证

- 定向 Rust 测试、全 workspace 格式检查与二进制构建。
- 隔离 Server、Runner、CLI 及固定 App Server 输出验证父键及 Task fallback，按两端 DB 检查作者继续收尾、审阅排队、双向停妥顺序与其他目录竞争者。覆盖原 HEAD 已前进时的精确 Git 对象及文件副本。
- 两轮相同 Issue/Task/Reviewer/assignment，核对候选、范围、结论、操作与原生 thread。复验重复调用、丢失同步回执、断链、未知恢复、明确不可恢复后的持久历史接续。
- 运行既有 `external_wait`、原生审阅、资源与协议窗口回归。合成 fixture 与实际模型/原生运行分别报告。

## 独立审阅后的边界回归

- 在 Server 应用审阅请求的事务内核对作者 Assignment、停止控制和双方 Issue 的祖先状态。已取消的迟到请求保留本轮回执，不再启动原 Review Task。
- 已发放且缺少共享目录证据的旧式在途审阅不做专门升级续跑兼容或回填。协议 v1/v2 窗口及共享审阅能力协商不变。
- 同机接替后显式复审使用原 Review Issue/Task 和原具名 Agent。新的 Runner 分配必须等原审阅 Assignment 的停妥交接证据。接续使用 Server 持久保存的审阅历史，不搬运原生线程，也不因普通讨论唤醒已结算审阅。
- 增加取消竞态及同机接替复审的进程回归，核对数据库中的回执、原身份和实际 Run 顺序。
