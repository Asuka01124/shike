# 拾刻

Windows 奖励定时领取工具。登录B站一次，添加奖励并选好时间，到点自动领取；结果保存在“领取记录”中。采用 Rust + GPUI 原生界面。

## 使用

1. 运行 `拾刻.exe`，点击“登录B站”，在打开的官方网页完成登录。软件会记住登录状态。
2. 点击“添加奖励”，粘贴具体奖励的领取页面链接。
3. 选择“明天零点”“现在领取”或“自选时间”。所有时间均为北京时间。未登录时也可以先安排未来的奖励；现在领取需要先登录。
4. 到点后会自动领取，成功后进入“领取记录”。有兑换码时可以直接复制，也可以打开领取页查看。
5. 关闭窗口后仍会继续等待。双击系统托盘中的拾刻图标可重新打开；点击“退出拾刻”或托盘中的“退出软件”才会结束程序。

请复制某个具体奖励的领取页面地址，活动海报首页不能用于领取。同一份奖励只需添加一次，已领取的奖励不会重复执行。

程序需在运行状态、电脑开机且有网络时执行。等待任务时会请求系统保持唤醒，但手动休眠、合盖或关机会中断。未设置开机自启动。重启程序后会恢复尚未到期的任务；明显错过时间的任务可编辑时间后继续。

## 本地文件

所有数据保存在程序同目录的 `data/`：

- `tasks.json`：账号会话、任务和归档结果。
- `tasks.backup.json`：上一次保存的副本。
- `browser-profile/`：独立登录浏览器配置。

Cookie本地保存，不依赖Node.js、Python或远程应用服务器。登录浏览器优先使用本机Chrome，找不到时使用Edge。登录态失效时点击“重新登录”。

领取接口仍由B站判定资格、库存和验证要求。遇到验证码或明确限流会停止该任务，显示原因；网络超时会保留为未完成，便于核对领奖记录。不会自动解决验证码。

## 构建

需要Rust 2024工具链、Visual Studio C++构建工具和Windows SDK（包含 `fxc.exe`）。当前环境可执行：

```powershell
.\build.cmd build --release
.\build.cmd test --lib --no-default-features --target-dir target-tests
```

其他环境可在VS开发者终端运行 `cargo build --release --locked`；必要时将 `GPUI_FXC_PATH` 指向SDK中的 `fxc.exe`。

`--demo` 只显示带示例任务的界面，不加载真实账号执行任务；`--probe` 只请求匿名任务信息并写入 `rust-api-probe.json`。

## 参考

- [GetLiveAward](https://github.com/yuzeeesama/GetLiveAward)：参考登录会话、WBI请求签名、接口参数和双线程组织方式，Rust后端为独立实现。
- [GPUI](https://github.com/zed-industries/zed)：原生桌面渲染。`src/input.rs` 的基础文本编辑实现改编自GPUI 0.2.2输入框示例，遵循Apache-2.0许可。
