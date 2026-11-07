[English](README.md)

# xtcc

为开发环境临时绕过 macOS TCC 检查。
Rust 控制层管理命令、连接、期限和日志。
Objective-C 注入库保留上游的私有接口实现。

独立的 `ne` 命令管理 Network Extension 系统扩展与网络配置。
在关闭 SIP 的开发设备上，可以批准已注册的扩展，并启用不使用 Apple 开发者证书、仅做 ad-hoc 签名的 Content Filter、DNS Proxy 和 Packet Tunnel VPN。

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
- `libxtcc_inject.dylib`：已做 ad-hoc 签名的 TCC 注入库。
- `libxtcc_ne.dylib`：已做 ad-hoc 签名的短生命周期 NE 客户端库。

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

## 管理 Network Extension

NE 操作要求 root，并要求关闭 SIP 的文件系统与调试限制。程序不会修改 SIP、AMFI、启动参数或开发者模式，也不会附加正在运行的系统守护进程。私有 API 由新建的短生命周期 Apple 客户端调用。

先将签名完整的宿主应用安装到 `/Applications`，再由宿主提交正常的 `OSSystemExtensionRequest`。扩展进入等待用户批准的状态后，执行以下命令。将示例标识替换为 `ne list` 显示的 bundle identifier。

```sh
sudo ./target/release/xtcc ne list
sudo ./target/release/xtcc ne approve com.example.FilterApp.provider
sudo ./target/release/xtcc ne filter com.example.FilterApp.provider
```

`approve` 只批准指定的 NE 系统扩展，不负责注册扩展或批准网络配置。`filter`、`dns-proxy` 和 `vpn` 无需确认弹窗即可创建或更新宿主的配置。命令从真实代码签名获取 provider 的 designated requirement；未签名或签名损坏的 bundle 会报错。

三种创建命令均接受可选的 `--vendor-configuration`。提供 plist 字典时替换 vendor 配置；省略时保留原值。字典内容由 provider 定义；如果 provider 不需要 vendor 配置，省略以下示例中的参数及文件路径。

### 配置 Packet Tunnel VPN

按 provider 文档准备 `vpn.plist`。将示例 bundle identifier 和服务器地址替换为 provider 的实际值，再批准扩展并创建 VPN 配置：

```sh
sudo ./target/release/xtcc ne approve com.example.VPNApp.provider
sudo ./target/release/xtcc ne vpn com.example.VPNApp.provider --server-address vpn.example.com --vendor-configuration vpn.plist
```

创建配置不会建立连接。将以下 `VPN_UUID` 的值替换为命令返回的 `configuration.identifier`，不能使用扩展的 bundle identifier。也可以用 `ne list` 查找配置 UUID。

```sh
VPN_UUID='replace-with-configuration-uuid'
sudo ./target/release/xtcc ne connect "$VPN_UUID"
sudo ./target/release/xtcc ne status "$VPN_UUID"
sudo ./target/release/xtcc ne disconnect "$VPN_UUID"
```

`connect` 等待 `connected`，`disconnect` 等待 `disconnected`；断开后配置仍保持启用。使用 VPN 前，必须验证经过 provider 所设路由的实际流量。xtcc 不实现 VPN 协议，也不获取凭据。

按需要管理已保存的配置：

- **停用配置**：运行 `sudo ./target/release/xtcc ne disable "$VPN_UUID"`。
- **重新连接**：配置停用后，先运行 `sudo ./target/release/xtcc ne enable "$VPN_UUID"`，再运行 `sudo ./target/release/xtcc ne connect "$VPN_UUID"`。
- **删除配置**：运行 `sudo ./target/release/xtcc ne remove "$VPN_UUID"`。

### 配置 DNS Proxy

按 provider 文档准备 `dns-proxy.plist`。替换示例 bundle identifier，再批准扩展并创建 DNS Proxy 配置：

```sh
sudo ./target/release/xtcc ne approve com.example.DNSProxyApp.provider
sudo ./target/release/xtcc ne dns-proxy com.example.DNSProxyApp.provider --vendor-configuration dns-proxy.plist
```

macOS 按启用的配置启动 provider。DNS Proxy 不使用 `ne connect`、`ne disconnect` 或 `ne status`。将以下 `DNS_UUID` 的值替换为返回的 `configuration.identifier`，然后查看、停用或重新启用配置：

```sh
DNS_UUID='replace-with-configuration-uuid'
sudo ./target/release/xtcc ne list
sudo ./target/release/xtcc ne disable "$DNS_UUID"
sudo ./target/release/xtcc ne enable "$DNS_UUID"
```

必须验证 UDP 和 TCP DNS 查询符合 provider 的预期规则，并检查 provider 的启动和流量日志。仅有 `enabled: true` 或普通 DNS 查询成功，都不能证明代理已接管流量。删除配置用 `sudo ./target/release/xtcc ne remove "$DNS_UUID"`。

### 查阅命令与限制

| 命令 | 对象与效果 |
| --- | --- |
| `ne list` | 只读列出 NE 系统扩展与配置 UUID。 |
| `ne approve <extension>` | 按 bundle identifier 批准一个已注册的 NE 系统扩展。 |
| `ne filter <extension>` | 创建或启用 Content Filter。 |
| `ne dns-proxy <extension>` | 创建或启用 DNS Proxy。 |
| `ne vpn <extension> --server-address <address>` | 创建或启用 Packet Tunnel VPN 配置，不建立连接。 |
| `ne connect <uuid>` | 连接已启用的 VPN，并等待系统报告 `connected`。 |
| `ne disconnect <uuid>` | 断开 VPN，并等待系统报告 `disconnected`。 |
| `ne status <uuid>` | 通过 SystemConfiguration 读取 VPN 实时连接状态。 |
| `ne enable <uuid>` | 启用已有的 Content Filter、DNS Proxy、VPN 或 App VPN 配置，不连接 VPN。 |
| `ne disable <uuid>` | 停用指定配置，不卸载扩展。 |
| `ne remove <uuid>` | 删除指定配置，不卸载扩展。 |

- **NE 更改会持久保存**：不使用 TCC 会话期限，`xtcc stop` 也不会撤销。必须使用 `ne disable` 或 `ne remove` 清理。
- **VPN 创建范围**：支持 Packet Tunnel 系统扩展，不包含 IKEv2 凭据配置或 App Proxy VPN 创建。不支持 `.appex` 激活。
- **状态不等于流量验证**：`enabled` 与扩展的 `active` 描述配置和注册状态，不证明 provider 已启动。`connected` 也不能代替实际流量检查。禁用 VPN 后，系统可能移除连接服务，此时 `ne status` 会报告服务不可用。
- **重复操作保持幂等**：系统明确返回「配置未变化」时，命令成功并返回 `unchanged: true`；其他错误仍会失败。同一 VPN provider 匹配多个配置时，命令会拒绝写入，不会任意选取。
- **超时后检查系统状态**：先运行 `ne list`，VPN 还必须运行 `ne status`，再决定是否重试写入。`connect` 超时后，系统可能仍在连接；使用 `ne disconnect` 取消连接。
- **超时与输出**：`ne --timeout <seconds>` 默认限制每个客户端运行 30 秒，范围为 1 秒至 300 秒。默认输出可读 JSON，`--json` 输出紧凑 JSON，不输出 vendor 配置或凭据。

没有 Apple 开发者证书时，必须先用 `codesign --sign - --entitlements <file>` 签名内嵌 provider，再签名宿主。两者仍须具备正确的 NE entitlement、bundle 结构、Mach service 和 app group 声明。

程序不会重签第三方应用，也不会补齐 entitlement、描述文件或基于 Team ID 的 app group 和钥匙串访问权限。provider 更新或重签后，必须由宿主重新激活，再运行对应的创建命令更新代码要求。NE 库可通过 `XTCC_NE_LIBRARY` 指定，查找规则与 TCC 库相同。

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

NE 运行验证覆盖 macOS 27.2 上自建的 Content Filter、DNS Proxy 和 Packet Tunnel VPN 夹具，包括实际过滤、UDP/TCP DNS 应答与隧道收发包。
验证条件与缺口记录在 [NE 验证记录](VALIDATION.md#network-extension-validation)。
真实 TCC 注入、第三方 VPN 协议、其他 NE 类型的流量、跨系统兼容性与系统服务故障恢复仍未验证。

# Acknowlodge

感谢 [unixzii](https://github.com/unixzii) 的
[tcc-bypass](https://github.com/unixzii/tcc-bypass)，其设计与实现为本项目的 Rust 控制层重写提供了基础。
参考提交为 `78da98104bafff7ecb96935cf198cb4196a28ffd`。

以下 Objective-C 文件保留自该提交：

- `native/tccd_inject.m`
- `native/Attribution.m`
- `native/Attribution.h`
- `native/ControlProtocol.h`

服务与日志标识改为 `dev.xtcc`。
注入库导出的 `cyan_tccd_*` 符号保持不变。
Rust 控制层沿用原协议和生命周期行为。

参考提交未发现 LICENSE 文件。本仓库保留上游代码不代表获得再分发许可。
发布或分发前，请先向原作者确认授权。
`Cargo.toml` 设置了 `publish = false`。
