# 一期收尾与验收

一期沿 [总体设计](../design.md)、[执行与等待](../task-lifecycle.md) 和 [Server 与 Runner 契约](../server-runner.md) 的已定边界推进。每段改动直接进入主干，分别记录固定 fixture 与实际服务读回的验证范围。

## 外部事实、等待和批次

- Server 将 GitHub 的 PR 状态、当前 head 的 CI 汇总及有明确身份的检查保存为带版本的事实。投递保存原订阅、等待和事实身份，固定五分钟窗口以首条待投递事实为起点。重复观测只更新观测时间。
- Runner 本地持久接受带具体条件的等待。回合实际停妥后释放 slot，原 Task 保留目录占用，其他目录照常推进。已有匹配事实和权限失败都沿原等待送回同一责任 Agent。普通用户输入继续及时唤醒，Owner 可撤销尚无结果的等待。
- 固定 fixture 核对同一 Runner 上实际停止、原 Task 续跑、新 Run、保留目录、过期检查拒绝匹配和单次投递。测试推进已持久化的批次截止时间，部署保持五分钟默认窗口。

## 后续验收块

- 补齐 Queue、Steer、取消、重开与实际停妥回执。
- 独立运行 Server、Runner 和 tsnet，核对同机交接、原目录和原 Run 归属，以及 Server 的协议预检。
- 在真实本机 Codex App Server 和可用的可信通道核对完整主链、Attention、并行任务和回传。记录部署验证与固定 fixture 的各自范围。

## 回合控制

- Server 持久登记 Stop 与 Steer。Stop 在原 Runner 证实停止后结算；Runner 离线时保留待送控制。Steer 关联精确 Run 和原生回合，并向 App Server 提供本次操作 ID。未送达和回执未知分别保留，用户可显式把未送达的原文改为 Queue。
- Runner 在执行循环检查本地控制表。普通同步和 Task 全量对账共用相同操作与回执。运行中 Stop 使用原生中断并验证进程停妥；排队或等外部事实的 Task 直接结束本地安排。
- Issue 业务取消与重开使用 Issue 状态及明确原因。Owner 能为指定子 Task 提交停止，旧控制沿旧 Task 结算。用固定回合验证活跃 Steer、旧 Run 未送达、停止和回执重放。

CLI 可用 `tend task stop <task-id>`、`tend run steer <run-id> <text>`、`tend control read <operation-id>` 与 `tend control queue <operation-id>`。直接发出的 `tend issue cancel` 为当前树内已知 Task 登记停止控制；Owner 也可按需提交具体 Task 停止，并独立标记 Issue 业务状态。
