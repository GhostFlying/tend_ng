# 部署分类目录与运行期更新

## 现状和目标

本分支基线的 Tend NG Runner 只在进入 main 时调用 `Config::load`；具名 Agent 没有运行中写 API。周期同步已重建 RunnerProfile，Server 在内容改变时立即更新持久快照，未改时合并心跳写入。Router、Owner 和 CLI 已有候选查询，候选与实际分配都使用具名 Agent。旧 Tend 的 Runner Agent control 是另一套产品路径，不是 Tend NG 的写入口。保留心跳、调度、异步受理、原 ID 领取和团队同机契约。

## 设计及实施

1. Server 可选部署 JSON 文件。默认含既有五类及独立 Server 本机目录项。有效文件发布整个分类快照，配置无效保留上次好版本。稳定 ID 绑定封闭角色及角色允许的范围。Coordinator 只可顶层，Review 只可委派，Work 可选其一或皆可。专用独立审阅与默认直派仍使用原 ID。Runner 不自行解释任意字符串或 Server 本机类别。
2. Runner 同进程复读主配置内的具名 Agent 和 launcher，不热变实例 ID、Host、槽位或仓库等启动字段。通过现有心跳更新已有 Server 快照，Server 在同步回执返回权威类别。两个方向都用向后兼容的能力字段，避免旧端领取或应用无法解释的工作。部署先配置目录，再在所需 Runner 配置具名候选。以原 assignment 保存角色与启动配置；已接受的工作无需当前目录或具名候选仍能在同一 Task 和 session 续跑。
3. 复用 Router/Owner 候选工具，增加无类别查询及 CLI/API 分类目录。类别参数为稳定字符串，不把当前部署值写进原生工具 schema。保存的 operation、已发未确认分配按原身份结算；已创建 Task 的预留、领取和同机接替继续使用原类别；无合规候选不回退，返回类别、候选或老版本限制原因。已有的顶层／子项执行约束照旧。
4. Rust 定向测试角色、兼容、配置与派发状态；用隔离真实 Server/Runner 和固定 App Server 输入，核对进程及原生 thread/Task 身份、顶层和同 Host 委派、描述及候选更新、停用、等待续跑，以及丢回执和实际前代 Runner。不手工部署、修改生产配置，或跑生产测试。按本仓门禁做独立审阅、合入并确认完整远端 SHA。

运行期保证适用于使用新版稳定工具接口的 session，不随配置更新换 thread、Task 或进程。验证发布前旧 schema 的会话能力并明确跨版本边界。

## 隔离验收

- `tests/e2e/runtime_agent_categories.py` 启动真实 Server、CLI、Runner 和隔离原生协议 fixture。用同一活跃 Router turn 和 Owner turn 观察新增、修改和停用后的结果。验证顶层分配、同 Host 子项、角色工具拒绝、无候选不回退、停用后的原委派收执重放，以及移除原候选和 launcher 声明后，通过 Attention 在原 Task 和 thread 使用已保存的 launcher 续跑。
- fixture 丢掉 Server 已持久接受的那次 HTTP 确认，并核对两端只保留一个分配身份。定向 Rust 测试另外覆盖已建 Task 的未发预留可继续领取及已发、未确认契约跨停用和过期后的原样重送。
- CI 使用前版的心跳线型。隔离验证也可用 `--legacy-runner <前版二进制>`，确认前版真实 Runner 会接经济档，即使报告了同名的自定义档也不会收到自定义任务。使用前版 Server 和新版 Runner 运行 `tests/e2e/runner.py --legacy-review-capability`，验证仅内置 review Agent 具备所需能力时，协调任务可领取，丢失回执后仍沿原分配恢复。
- 进程及会话证据来自固定输入和隔离 ledger，不使用真实模型或生产配置。活跃回合跨配置变化保留原 App Server PID；等候后的续跑沿现有进程释放生命周期复用原 thread，不人为维持空闲 Codex 进程。

## Task 受理与配置版本

配置在启动时加载，运行中默认每 60 秒检查一次，心跳和任务领取仍按原频率。分类停用阻止新 Task 进入该档，已创建的 Task 保留原分类和已选身份。覆盖排队、预留、首次领取、等待续跑及同 Host 的换代接续。使用旧 Server 的内置能力匹配、丢回执后的返回值相等、停用前后创建任务作为定向回归。

具名 Agent 同 ID 的 CLI、harness 启动参数、模型、effort 和 Codex home 可以在线更新。Server 在新 Task 进入时记录当时各可用 Host 的匹配版本，Runner 在发布版本前保存 Host 共享的可执行配置。原 Task 的预约、领取、等待续跑与同机接替继续取得原配置，尚无合格候选时以首次匹配为准。用同名配置 A/B 验证旧任务与新任务的真实启动参数和模型。

已有匹配版本、尚未选定主机的顶层 Task 可以分配给晚到的 Host，只要它提供同一版本。两代并存时，后继可以热改具名配置；排空和退役按此前的接替记录继续。用 `tests/e2e/runner_upgrade.py` 验证旧代在运行、新代更新配置、升级命令重试及退役。

Runner 本地首次受理委派或独立审阅时，按当时已同步的目录检查启停，并随工具回执保存已选中的同机具名配置。断线期间的原受理随原操作 ID 补传。N−1 Runner 缺少目录能力，其新建子项由 Server 收件时判断，停用时返回终止原因。复审沿原审阅 Task。待安排记录保留意图，显式开始和退回待安排后重新开始均采用当时生效的分类。
