# Issue 执行连续性

本文记录首次执行连续性交付。独立审阅目录与后续轮次的现行契约见 [绑定审阅复用作者目录与同 Task 复审](shared-workspace-review.md)。

## 目标与边界

以 Issue #8 的既有事实为只读基线。一次明确交付尽量由原作者 Task 从调查做到反馈吸收，独审仍由责任 Agent 判断。父级保留全部源事实，普通唤醒固定五分钟合并。兼容续跑优先复用同一个 Task 的 Codex thread。

不写入 #8，不操作运行中的 Tend 或 8832，不使用个人 Codex home。本次在一个子 Issue 交付，无部署授权。只在出现真正独立的交付时新增 Task。

## 实施顺序

1. 追踪实际注入的角色指引、现有父级收件与同步、审阅和 Runner 回合恢复。
2. 在 Server 持久化普通输入的每 Task 窗口与到期释放，保留即时路径。
3. 增加候选绑定的独立审阅、原作者等待及回传，复用现有分配、写入锁及业务回执。
4. 在原停妥边界之后尝试恢复兼容 thread，并用既有持久事实回退。
5. 更新契约，运行边界及进程 fixture，对实际候选独立审阅，记录验收后 rebase、推送和读回主干。

## 验证矩阵

| 路径 | 验收 |
| --- | --- |
| 普通父级输入 | 固定时钟、不延展、事实即时保留、重启及重试、一次唤醒及完整输入 |
| 即时输入 | 用户答复、控制、明确阻塞与普通窗口独立 |
| 独立审阅 | 原候选和审阅来源可追溯、同作者 Task 收件、释放 slot、审阅无法接管共享 workspace |
| 同 Task 恢复 | 同 Runner/配置/workspace 续用、明确不可用时新 session、旧执行未知时不启动 |
| 共享接口 | cargo test 及受影响隔离 Server/Runner/CLI e2e |
| 真实模型 | 仅用独立且已授权的 Codex home。否则记录原因，不用 fixture 冒充模型测量 |

风险集中在旧执行归属、持久收件和停妥竞态、混合版本下的工具支持、五分钟内的可见性与误唤醒。验收记录在本文补齐。

## 验收场景与计数

Issue #8 仅使用委托提供的只读基线。它有 7 个工作子 Issue / Task（调查 1、实施 3、审阅 3），每项 1 个 Run。父协调 Task 有 8 个 Run、8 个不同的原生 thread。本次未连接或写入该 Issue，也未操作正在运行的 Tend。

隔离验收使用真实 Server、Runner 与 CLI，模型输出由 App Server fixture 提供。作者先写候选，提交文件并请求独立审阅。审阅者提出修改，作者在原 Task 内吸收并结案。父级计数不含首次开工。

| Fixture | 工作子 Issue | 父级唤醒 | 不同原生 thread |
| --- | ---: | ---: | --- |
| 常规续用，Server 重启及同步回复丢失 | 2 | 1 | 作者 1 / 2 Runs，父级 1 / 2 Runs |
| Rollout 缺失、损坏，或 App Server、模型和 effort 配置变化 | 每种 2 | 每种 1 | 作者 2 / 2 Runs，父级 1 / 2 Runs |
| 原 thread 返回 active、Runner 重启、缺少状态，最终明确 idle | 2 | 1 | 作者 1 / 2 Runs，父级 1 / 2 Runs |
| 同目录额外 Task 竞争探针 | 3（含一个探针） | 1 | 作者 1 / 2 Runs，父级 1 / 2 Runs |

普通窗口收录 5 条事实，加入竞争探针时为 6 条。Server 重启后截止不变，保留原 ID，首次回复丢失后重传并由同一 Run 消费。测试时推进临时 SQLite 的截止，生产窗口仍固定五分钟。Rust 定向用固定时钟精确检查窗前、到期、相邻窗、另一个 Task 的独立窗口、重复入账及超过单批条数的整组合并，另核对用户留言和转发、Attention、停止及明确失败不受普通窗口拦截。

审阅在一个 slot 下运行，作者实际停妥并归还 slot，但继续独占作者的共享目录。审阅者使用独立目录、按完整 SHA 检查 detached HEAD，并读取快照副本。竞争 Task 等作者结束才拿到同一目录。审阅工具重放不产生第二条输入，结构化结论只投给原作者 Task。在 Active 分支，Runner 重启后仍用原预备 Run 保留未消费输入和目录，必须收到同一 thread 的明确 idle 才提交回合。

## 独立复核

本作者 Task 内使用 Codex 原生子线程 `01a2116c-55ce-7bf1-8004-e4e99c7891f4` 对冻结候选及后续差异做只读复核。它复跑了损坏 rollout、配置变化和协议窗口 fixture，发现并推动修正了回退丢输入、旧 App Server 启动配置、Git 候选绕过、降级交接无法接续，以及 Active 证据写入顺序与 idle 确认。最终功能 tree `2f2f128b53a9c9413fec0b5b92be522bb7da90f6` 的最后差异无交付阻塞。当前 Issue 的 `tend_record_review` 未识别该 native 子线程，因此在此保留复核来源和结论，不宣称已生成 Tend 原生审阅记录。

## 未验证与已知边界

- 未运行真实模型的 #8 类自然语言任务。没有显式登记、可用且单独授权的 Codex home，也未使用当前会话的 Codex home 或其凭据。模型自主拆分、选审和真实原生恢复比例未测。原生子线程代码复核不等于该模型实验。
- 审阅 workspace 隔离指 Tend 调度和工作目录写入归属，沿用可信、无 OS 沙箱运行的约定。
- v2 保证超过旧单页上限的普通窗口整组发布。v1 可继续普通同步，但旧 Runner 对大组仍可能逐页处理。极端大窗中，大量输入 ID 仍可能触及已有 256 KiB 同步请求限制。此上限未做容量改造。
- 换 Runner 后不迁移原生会话。审阅若尚未运行且后继拿不到原 Runner 的文件副本，会按原审阅 Task 汇报准备失败，而不是访问作者工作区。若同一 thread 持续无法确认 idle，保留原 Task、预备 Run 和写入归属，等待可核实事实。

## 执行记录

- 使用仓库 devcontainer 的 Rust 1.91.1、只读的最终功能 tree 和离线依赖缓存，通过 `cargo fmt --all -- --check`、`cargo test --locked --offline` 和 `cargo build --locked --offline --bins`。
- 通过 28 个受影响的隔离进程场景，覆盖 execution_continuity 六种模式、coordination 五种收尾、协议窗口、Issue Chat、Steer/Stop、PR 看板和发现、原生审阅、Run/Runner 重启、外部等待与接替、Attention 四种方式、交付与多份报告。
- 另对 rebase 后的 Issue 详情运行只读 API fixture，鉴权、历史报告、文件出处及决定引用均通过。额外的浏览器 e2e 未执行至浏览器阶段，离线 devcontainer 没有已构建的 `web/dist`。
