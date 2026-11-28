# 同 Host Runner 换代续接原 Codex thread

## 目标与边界

保留同一 Task 已停妥的 Codex 上下文，同时保留换代各自的 assignment、Run 和 turn 身份。旧 Run 正常结束不是 Issue 结案。前驱仍按原身份提交停妥、操作与原件。后继沿现有 workspace 独占权领取，不能重复执行旧 turn。#149、#152 只作为现象证据，不修改其历史。跨 Host、换 Agent 与换 workspace 沿原规则，不主动切生产 Runner、不手动部署。

## 实施

1. 扩展可选交接上下文，携带已停妥 native binding。前驱在真正启动时冻结本地 workspace、专用 home 和执行配置的身份，确认 native turn 停妥并收到原执行和操作回执后，随原交接记录移交。Server 从原已领取合同、相同 Host 的接替关系及已收妥的 Run 核对来源，原件未同步时暂不发放 binding。未经执行的连续换代只转交已获验证的原 binding。旧字段省略时仍按原规则交接，不回填旧 Run。
2. 后继使用已独占的目录和自己选定的具名 Agent、启动器及 home，验证目录路径和 device/inode、冻结执行兼容键。不得用交接路径做启动输入。验证成功后先 resume，按原 turn 核对已停妥的 native 历史，再以新 Run 为 client ID 开启一个新 turn。沿原 Task 的既有收件和操作回执推进。兼容键或目录身份不符及 App Server 明确报告原 thread 不存在时，记录明确原因并用已有持久工作事实开启新 session。
3. 暂时启动失败、RPC 或回包失败、active 或原回合尚无法对账时，保留同一 prepared Run 与 workspace，按原身份重试。新 turn 一经可能发出，只按已知 turn ID 或原 Run client ID 查证，不再次提交 turn/start，也不换 thread 逃离未知状态。原执行历史、文件和回执的位置与业务语义不变。
4. 补齐 Server/Runner 与 native rollout 契约。可选字段不改变 v1/v2 窗口。证据不足的存量按既有新 session 路径处理，不只凭路径或 thread ID 猜测。

## 验证与交付

- 在开发容器跑定向 Rust、协议窗口、同机滚动接替、外部等待与审阅边界回归。
- 用隔离真实 Server、两个真实 Runner、真实 Codex App Server、回环模拟模型上游及独立验收 home。按身份核对原等待、排队输入、操作和原件、workspace stamp，确认两代 Run 和 turn 不同、同一 native thread 且无重跑或重放。故障注入核对暂时障碍、丢回复、不可恢复回退及旧字段缺省。
- 对精确候选做独立审阅，完成必要修订和定向验证。按仓库约定合入 origin/main，记录完整远端 SHA、对应 CI 与隔离验收证据。不延伸到手动安装、发布或生产切换。

## 首轮独审后的修订

- 区分 App Server 报告原 thread 仍 active 与已经报告 idle、但前沿读取失败。只有前者保留 active fence。idle 前沿不可读时既不提交 turn，也不妨碍 Stop 停妥尚未启动的后继 Run。
- 同机换代不改变独立审阅协议。原审阅已结算而下一轮显式请求仍在队列时，保留 Review Issue、Task、具名 Reviewer、停妥与历史，但不把上一轮的 thread 转给新一轮。测试比较在前驱排队和由后继作者发起两种时序。
- 在明确允许 fallback 后，先持久登记分配空 thread 的 App Server 进程。若 thread/start 回包丢失，只有确认该进程已停且尚未提交任何 turn 后才重新分配。若已把 thread 和进程绑定到可能提交的 turn，则仍只沿原 Run 执行重查。
- 基线已支持具名启动器冻结后，兼容检查使用已领取合同中的 Agent／角色和冻结 launcher，不从新目录的当前配置选择旧 Task 的执行身份。归档换代同时验证后继热更新只影响新 Task。
