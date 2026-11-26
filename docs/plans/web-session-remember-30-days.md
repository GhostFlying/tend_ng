# Web 记住此浏览器 30 天

- 目标为仅对新签发、明确勾选「记住此浏览器」的 HTTPS 会话，将 Server 绝对有效期和 Cookie `Max-Age` 同步调整为 30 天（2592000 秒），同时更新登录页和现行契约。
- 不改数据库 schema、迁移、会话读取或回收逻辑。已签发会话仍按保存的 `expires_at` 到期，不重写、不续期。未勾选和本机回环 HTTP 仍为 12 小时（43200 秒）且不设 `Max-Age`，只有 HTTPS 可以勾选。同入口换登继续撤销出示的旧会话，退出继续撤销当前会话。
- 验证运行 `cargo test --locked -p tend-server web_session`、`cargo test --locked` 和 `cargo build --locked`，以及 `pnpm --dir web build` 和 `pnpm --dir web test`。再用 `python3 -B tests/e2e/web_session.py target/debug/tend-server target/debug/tend target/debug/tend-runner --web web/dist --screenshots <临时目录>` 运行临时 SQLite、TLS 入口和 Chromium profile。
- 隔离 fixture 检查新会话的 Cookie 属性、精确 TTL、换登/退出撤销及本机回环。模拟仍有效的旧版 7 天记录，经过新登录、Server 重启和只读检查后比较原始记录，验证未延长、未删除。确认 Chromium 看到 30 天文案和持久 Cookie，附上实际渲染截图。不访问或部署运行中的服务。
