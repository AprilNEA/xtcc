# xtcc

为开发环境临时绕过 macOS TCC 检查。
Rust 控制层管理命令、连接、期限和日志。
Objective-C 注入库保留上游的私有接口实现。

**本项目仅适用于你有权管理的隔离开发设备。**
它不是普通应用的权限申请方案。
它不会让 SIP 开启的系统自动满足运行条件。

## 当前重写范围

Python 外层启动器与 Objective-C helper 改为 Rust。
注入库仍由 Apple Clang 编译为 `arm64e`。
两者通过 Mach 消息通信。

LLDB 内部仍执行 Python 脚本。
因此，本项目不是完全移除 Python 的方案。
它也不是完整的纯 Rust 实现。

## 环境要求

- Apple Silicon Mac。
- Rust 与 Cargo。
- Xcode，包含 macOS SDK、Clang 和支持 Python 的 LLDB。
- 系统提供 `make` 与 `codesign`。

原生库的最低部署目标为 macOS 15.0。
这不代表已经验证所有 macOS 15 及以后版本。
私有 TCC 接口仍取决于具体系统版本。

运行控制操作要求 root。
注入还要求系统允许调试附加和加载此库。
上游要求关闭 SIP；本项目不会替你修改 SIP。

## 构建与离线检查

```sh
make
make check
./target/release/xtcc --help
./target/release/xtcc doctor
```

构建产生以下文件：

- `target/release/xtcc`：Rust 命令行程序与 broker。
- `libxtcc_inject.dylib`：已做 ad-hoc 签名的注入库。

构建不会加载库，不会启动 broker。
测试不得附加 `tccd` 或改变 TCC 权限。
Mach 回环测试只使用测试自身创建的端口。
原生 ABI 夹具只比较内存布局与消息编码。

默认 `CARGO` 指向 `$HOME/.cargo/bin/cargo`。
如果安装位置不同，可执行 `make CARGO=/path/to/cargo`。

## 命令

`start [minutes]` 临时启用检查绕过。
时长默认为 30 分钟，范围为 1–1440 分钟。
重复调用会复用健康的 broker，并更新期限。
`enable` 是 `start` 的别名。

`stop` 停止绕过，但不会卸载注入库。
`disable` 是 `stop` 的别名。
已加载的方法替换会保留到对应进程退出。

`status` 查询连接、状态、期限与日志计数。
`logs` 回放缓冲记录，并继续读取新记录。
`logs --snapshot` 只读取一次快照。
`logs --all` 包含默认隐藏的 Apple 归因记录。
`--json` 输出机器可读结果。

`doctor` 只检查工具位置与构建产物。
它不测试调试附加权限，不启用绕过。
程序不会隐式调用 sudo。

注入库可以通过 `XTCC_LIBRARY` 指定。
否则，程序查找可执行文件旁或源码目录的库。
请勿在运行期间替换程序使用的构建产物。

## 行为与限制

权限检查钩子保持上游逻辑。
注入端失去 broker 连接后停止绕过。
日志丢失不会阻塞权限检查。
所有日志仅存在于有界内存缓冲中。

默认缓冲为 1,024 条，序列化数据上限为 8 MiB。
单条消息体上限为 16 KiB。
慢速订阅者会收到日志缺口通知。
broker 退出后，日志不会保留。
broker 生命周期诊断写入系统 syslog。
这些诊断以 `xtcc:` 开头，不包含权限事件。

`BYPASS` 表示 hook 补充了 entitlement 检查结果。
它不代表最终权限请求一定成功。
默认隐藏规则仅影响日志显示，不限制绕过范围。

服务标识改为 `dev.xtcc.control`。
日志子系统改为 `dev.xtcc`。
不要与上游工具同时操作相同的 `tccd`。

## 验证状态

本轮验证以构建、静态检查和离线测试为限。
最终结果记录在 `VALIDATION.md`。
真实注入、跨系统兼容性与系统服务故障恢复未验证。

## 来源与许可

项目参照 unixzii/tcc-bypass 的固定提交重写。
保留的原生文件和来源详见 `UPSTREAM.md`。
原提交未发现 LICENSE，发布前需确认授权。
当前配置禁止通过 Cargo 发布。
