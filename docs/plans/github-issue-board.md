# GitHub 观测与只读 Issue 看板

Server 使用服务用户的只读 GitHub 权限。外部查询以关联或明确登记的对象为范围。Runner 的 Git 权限用于 workspace 操作。私仓需要由 Server 取得只读访问。 [设计](../design.md#pr-和-ci-事实的采集与投递)

## 第一段

- CLI 与 Work Agent 可以按 URL 将 GitHub PR 登记到 Issue。登记、可重试的操作回执和待观测对象一起持久化。不同 Issue 可以引用同一个 PR，对同一目标的重复登记沿原关联返回。
- Server 的后台观察线程调用本机 GitHub CLI，定期读取已关联 PR 的状态和当前 head 的 CI 汇总。网络读取在事务外。Provider 失败时继续保留链接和最近一次成功观察，界面显示观测暂不可用。
- Web 看板参照旧 Tend 的状态列和紧凑卡片，直接显示关联 PR 链接及已保存的状态。CLI 和 Web 调用同一分页 API。看板仅在 Server SQLite 中读取当页 Issue、当前 Task 和批量关联。运行中的工作和已结束的 Issue 分别呈现。
- 用固定 GitHub CLI 响应、真实 Server 和 Runner 验证显式登记、同一 PR 在两个 Issue 间共享观测、失败读取降级和旧库迁移。浏览器验证卡片链接、移动端排版和查看 Issue 的路径。

## 启动与验证

Server 默认在 PATH 中找 `gh`。使用 Server 服务用户的 `gh` 登录，或从该进程环境提供有只读权限的 `GH_TOKEN`。Runner 继续使用各自的仓库权限。CLI 用 `tend pr link <issue-id> <url>` 写入关联，用 `tend pr list <issue-id>` 和 `tend issue list main` 查看。 [GitHub CLI 配置](https://cli.github.com/manual/gh_help_environment)

```sh
cargo build --locked
pnpm --dir web build
python3 tests/e2e/pull_board.py target/debug/tend-server target/debug/tend target/debug/tend-runner --web
```

[桌面看板实拍](images/issue-board-1440.png) · [窄屏看板实拍](images/issue-board-390.png)

## 接续

从 Runner 上报的实际托管 repo 和工作分支补查 PR。工作 Task 终态和停妥后自动排一次。外部事件的订阅、等待与固定窗口投递共享同一来源，但由独立的持久投递负责。 [PR 关联](../design.md#pr-与-issue-关联) · [任务等待](../task-lifecycle.md)
