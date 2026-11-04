# xtcc 验证结果

首版 Rust 控制层已完成构建。
79 项测试全部通过，未执行真实注入。
最终验证时间：2026-11-04 07:02，UTC+08:00。

## 环境

- macOS 26.7，构建号 25G229。
- Apple Silicon，架构 arm64。
- Rust 1.98.1。
- Apple Clang 21.0.0，Xcode 27 SDK。
- Rust 与原生库的最低部署目标均为 macOS 15.0。

Cargo 声明最低 Rust 1.85。
本轮只验证 Rust 1.98.1，未验证最低版本。

## 已验证

`cargo fmt --check` 通过。
`cargo clippy --all-targets --locked -- -D warnings` 通过。
`cargo test --locked --offline` 通过。
测试包括 75 项模块测试和 4 项 CLI 集成测试。

`make` 成功生成两个产物。
`target/release/xtcc` 为 arm64 可执行文件。
`libxtcc_inject.dylib` 为 arm64e 动态库。
两者均通过 `codesign --verify --strict`。

CLI 帮助、版本和只读 doctor 已运行。
doctor 的文本和 JSON 输出均正常。
内嵌 LLDB Python 脚本通过语法检查。
该脚本没有被执行。

## 重点测试

协议测试覆盖消息长度、操作类型与非法输入。
原生 ABI 夹具使用项目实际的协议头。
它将 Objective-C 消息布局与 Rust 布局逐项比较。
测试还逐字节比较相同 JSON 消息的编码。

Mach 测试只创建本进程端口。
它们覆盖权利转交、引用计数和死亡通知。
它们还覆盖队列超时、非法消息清理和伪造通知拒绝。
测试不会查找或注册生产服务。

broker 测试覆盖握手、超时和完整性汇总。
日志测试覆盖容量、字节上限与回放边界。
订阅测试覆盖慢速读者、缺口和游标推进。
CLI 测试覆盖参数范围、别名和隐藏命令。
输出测试覆盖 JSON、过滤和控制字符清理。

## 未验证

真实 LLDB 附加、动态库加载与 readiness 未验证。
Rust broker 与真实注入端的完整会话未验证。
跨进程端口提取与生产服务注册未验证。
真实权限结果和系统服务崩溃恢复未验证。
跨 macOS 版本兼容性未验证。
后台 syslog 的系统日志展示未验证。

没有运行 `start` 或 `_serve`。
没有调用 sudo，没有更改 SIP。
没有修改 TCC 数据库，也没有发布项目。

## 相对上游的变化

外层 Python 启动器改为 Rust。
原生 helper 的控制逻辑改为 Rust。
LLDB 内嵌 Python 仍然保留。
Objective-C 注入实现只调整命名空间。
动态库增加稳定的 install name。

程序不再自动调用 sudo。
使用者必须显式决定是否以 root 运行。
参数校验先于运行时操作。
新增 doctor，只读检查工具与文件。

Mach 权利由 Rust 所有权类型管理。
握手后的回复固定完整 audit token。
请求增加连接代次与操作匹配检查。

broker 生命周期信息改用系统 syslog。
权限事件仍只进入有界内存缓冲。
端口提取错误的部分细分诊断尚未保留。

## 分发限制

上游固定提交没有 LICENSE 文件。
本项目保留来源记录，不附加推定许可证。
公开发布或再分发前，须确认原作者授权。
