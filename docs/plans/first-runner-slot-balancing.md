# 顶层 Issue 首次派发的 Runner 负载均衡

## 范围

只改变无显式 Runner、无前序 Task、无 Issue 树归属且不是交接续跑的顶层 Issue 首次派发。领取前被撤回、仍未绑定的同一首 Task 可重新匹配。沿用现有 Runner、Agent、协议、审阅和 workspace 资格过滤，不改变有归属任务的匹配顺序或预分配状态机。

## 实现

- 在 `assign` 的现有事务与资格过滤内比较合格候选。负载为 `(slot_limit - available_for_assignment + reserved/issued 数) / slot_limit`，按整数交叉相乘比较。普通分配仍要求上报可接单容量大于预留数。
- 等比例时优先选择最久未获此类自动预分配的 Runner，再以 Runner ID 打破初始并列。Server 迁移 35 新增轮换表，每个参与自动分配的 Runner 至多一条记录。历史不回填，不扫描累积的 assignment 历史。只有此类自动预分配会更新序号，且在预分配的同一事务内完成。迁移后，旧 Server 不能用原二进制直接启动这份数据库。
- `schedule_assignments` 每个任务仍使用独立的 `IMMEDIATE` 事务，后续任务读取同批先前已提交的预留。立即派发和待安排 Issue 的启动沿原事务调用相同逻辑。

## 验证

用隔离 SQLite fixture 覆盖不同容量的占用比例、等比例轮换与重开数据库、批内 `reserved` 和未确认的 `issued`、满载和无候选、显式指名与已有归属、代际与 Agent 资格，以及绑定审阅和 workspace 冲突。运行 Server 定向测试、Server 全量测试、Rust workspace 全量测试及格式检查。仅交付源码，不部署或用线上任务验证。
