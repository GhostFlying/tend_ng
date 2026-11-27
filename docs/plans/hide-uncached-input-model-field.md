# 隐藏 Token 模型明细的非缓存输入

## 目标与边界

- 全局 Token 用量页及每个 Issue 的模型明细在直属、含后代两种范围中都移除“非缓存输入”展示字段，桌面和手机一致。
- 只调整 Web 展示。底层用量、公共 API 的 `uncached_input_tokens`、费用估算、其他用量字段和费率明细保持不变。
- 不使用或改动用户截图中的示例 Issue，也不部署或写入生产数据。

## 实施与验证

- 摘要行及展开后的费率组件都不再渲染非缓存输入；费率组件按 API 固定顺序显式取值，避免隐藏首项后其余标签错位。
- 更新全局及 Issue 的真实页面用例，断言字段消失、其余字段和费用总额不变，并在 1440×1000、390×844 检查无横向溢出和控制台错误。
- 用隔离 Server、Runner 与 App Server fixture 验证 API 仍返回非缓存输入，直属与含后代小计及计价保持原值。
- 渲染原件 [桌面全局](images/hide-uncached-global-1440.png) · [手机全局](images/hide-uncached-global-390.png) · [桌面 Issue](images/hide-uncached-issue-1440.png) · [手机 Issue](images/hide-uncached-issue-390.png)。
