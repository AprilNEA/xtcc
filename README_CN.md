[English](README.md) · [验证记录](VALIDATION.md)

# xtcc

`xtcc` 是面向关闭 SIP 的 Apple Silicon 开发设备的单文件命令行控制工具，用于管理临时 TCC 权限绕过会话与 Network Extension (`ne`) 配置。构建阶段 (`build.rs`) 会自动编译并签名两个 `arm64e` Objective-C 载荷，将其内嵌至 `xtcc` 可执行文件，并在运行时按需释放到 `/private/var/run/xtcc`：

- **TCC 检查绕过（`start`、`stop`、`status`、`logs`）**：通过 LLDB（内置 Python 运行时）向 `tccd` 注入来自 [tcc-bypass](https://github.com/unixzii/tcc-bypass "前往外部站点") 的 Objective-C 钩子 (`libxtcc_inject.dylib`)，并由 Rust Mach broker (`dev.xtcc.control`) 管理会话期限与权限检查日志。
- **Network Extension 管理 (`ne`)**：独立于 TCC broker，通过注入 `libxtcc_ne.dylib` 的短生命周期 Apple 客户端进程批准已注册的系统扩展，并配置 Content Filter、DNS Proxy 和 Packet Tunnel VPN（支持无 Apple 开发者证书的 ad-hoc 签名 bundle）。

> **注意**：`xtcc` 仅限你有权管理的隔离开发设备使用，无法替代分发应用的正式签名、描述文件或用户授权流程。NE 控制已在 macOS 27.2 上通过自建的 Content Filter、DNS Proxy 和 Packet Tunnel 夹具完成真实流量验证；真实 TCC 注入、跨系统版本兼容性及系统服务故障恢复仍未验证，详情参见[验证记录](VALIDATION.md)。

## 快速开始

运行控制命令要求 Apple Silicon Mac（macOS 15.0 及以上）、`root` 权限 (`sudo`) 以及关闭 SIP 限制（TCC 注入还要求支持 Python 的 LLDB）。`xtcc` 禁止自动调用 `sudo`，也不会修改 SIP、AMFI、启动参数、开发者模式或正在运行的 NE 守护进程。

将发布包或本地 `make` 构建出的单个 `xtcc` 二进制安装到 `PATH`（例如 `/usr/local/bin`），然后运行 `xtcc doctor` 验证环境：

```sh
sudo install -m 0755 target/release/xtcc /usr/local/bin/xtcc
xtcc doctor
```

`xtcc doctor` 仅检查运行时工具路径（`xcrun` 与 LLDB）及内嵌载荷，不要求安装 Rust、Cargo、`make` 或编译器。检查以只读方式运行，无需 `root` 权限，不会写出动态库或附加系统进程。

运行目录与释放后的动态库权限均为 `0755`，不受调用进程的 `umask` 影响。

## 绕过 TCC 检查

使用以下命令启动限时绕过会话、查看 `tccd` 连接状态与权限日志，并在结束后停止绕过：

```sh
sudo xtcc start 30
sudo xtcc status
sudo xtcc logs --snapshot
sudo xtcc stop
```

运行期间需注意以下会话与日志行为：

- **会话期限与复用**：`start [minutes]`（别名 `enable`）默认启用 30 分钟，支持 1 分钟～1440 分钟。重复执行会复用健康的 broker 并更新到期时间。
- **停止绕过与失联保护**：`stop`（别名 `disable`）停止权限绕过，但不会从 `tccd` 卸载 `libxtcc_inject.dylib`，已替换的方法保留至目标进程退出。若注入端与 broker 断开连接，也会自动停止绕过。
- **日志读取与过滤**：`logs` 回放缓冲记录并持续输出新事件（`--snapshot` 仅读取当前快照后退出；`--all` 包含默认隐藏的 `com.apple.*` 归因记录）。过滤规则仅影响显示，不改变实际绕过范围。
- **内存缓冲上限**：日志仅驻留内存，缓冲上限为 1024 条记录与 8 MiB 序列化数据，单条消息体上限为 16 KiB。慢速订阅者会收到缺口提示，丢弃日志不会阻塞权限检查，且日志在 broker 退出后清空。
- **诊断与结果判读**：broker 生命周期诊断写入系统 syslog（前缀为 `xtcc:`，日志子系统为 `dev.xtcc`），不包含权限事件。`BYPASS` 记录仅代表钩子补充了 entitlement 检查结果，不代表最终权限请求一定成功。禁止将 `xtcc` 与上游 `tcc-bypass` 同时用于同一个 `tccd` 进程。

## 管理 Network Extension

`xtcc ne` 要求 `root` 权限并关闭 SIP 的文件系统与调试限制。它将「系统扩展批准」(`OSSystemExtensionClient`) 与「网络配置管理」(`NEConfigurationManager`) 分为两个独立步骤：

1. **由宿主应用提交扩展请求**：将签名完整的宿主应用安装到 `/Applications`，并由宿主发起正常的 `OSSystemExtensionRequest`，使扩展进入等待用户批准的状态。若不使用 Apple 开发者证书，必须先使用 `codesign --sign - --entitlements <file>` 签名内嵌的 provider bundle，再签名宿主应用；两者仍必须具备正确的 NE entitlement、bundle 结构以及 Mach service 或 app group 声明。
2. **使用 `xtcc ne` 批准并创建配置**：根据 `ne list` 输出的扩展 bundle identifier 执行 `ne approve <extension>`，再执行 `ne filter`、`ne dns-proxy` 或 `ne vpn` 免弹窗创建或更新网络配置。`xtcc` 从磁盘上的真实代码签名提取 provider 的 designated requirement（含 ad-hoc 签名的代码目录哈希），未签名或签名损坏的 bundle 会校验失败。

三个配置创建命令均可以传入 `--vendor-configuration <plist>`（字典格式的 plist 文件）：传入时替换已保存的 vendor 配置，省略时保留原有配置。

### 配置 Content Filter

列出已注册的扩展，按 bundle identifier 批准目标扩展，并创建或启用 Content Filter 配置：

```sh
sudo xtcc ne list
sudo xtcc ne approve com.example.FilterApp.provider
sudo xtcc ne filter com.example.FilterApp.provider
```

### 配置 DNS Proxy

批准已提交的 DNS Proxy 扩展并创建配置（若 provider 不需要 vendor 配置，可以省略 `--vendor-configuration` 参数）：

```sh
sudo xtcc ne approve com.example.DNSProxyApp.provider
sudo xtcc ne dns-proxy com.example.DNSProxyApp.provider --vendor-configuration dns-proxy.plist
```

macOS 会直接根据已启用的配置启动 DNS Proxy，因此 DNS Proxy 不使用 `ne connect`、`ne disconnect` 或 `ne status`。将返回结果中的 `configuration.identifier` 赋给 `DNS_UUID`，即可查看、停用、重新启用或删除配置：

```sh
DNS_UUID='replace-with-configuration-uuid'
sudo xtcc ne list
sudo xtcc ne disable "$DNS_UUID"
sudo xtcc ne enable "$DNS_UUID"
sudo xtcc ne remove "$DNS_UUID"
```

### 配置 Packet Tunnel VPN

批准 Packet Tunnel 扩展，并指定非空的 `--server-address` 创建 VPN 配置：

```sh
sudo xtcc ne approve com.example.VPNApp.provider
sudo xtcc ne vpn com.example.VPNApp.provider --server-address vpn.example.com --vendor-configuration vpn.plist
```

创建 VPN 配置时会自动启用配置，但不会发起连接。使用命令输出或 `ne list` 中的配置 UUID (`configuration.identifier`) 控制隧道连接：

```sh
VPN_UUID='replace-with-configuration-uuid'
sudo xtcc ne connect "$VPN_UUID"
sudo xtcc ne status "$VPN_UUID"
sudo xtcc ne disconnect "$VPN_UUID"
```

`connect` 会等待系统报告 `connected`，`disconnect` 会等待系统报告 `disconnected` 并保持配置处于启用状态。若已执行 `ne disable "$VPN_UUID"` 停用配置，重连前必须先执行 `ne enable "$VPN_UUID"`。

### 遵守运行限制

- **持久化与清理**：NE 配置持久保存在系统偏好设置中，不受 TCC 会话计时器或 `xtcc stop` 影响。清理配置必须使用配置 UUID 执行 `ne disable <uuid>` 或 `ne remove <uuid>`。
- **支持类型范围**：仅支持 Content Filter、DNS Proxy 和 Packet Tunnel 系统扩展，不支持 IKEv2 凭据下发、App Proxy VPN 创建或 `.appex` 激活。
- **签名与更新限制**：`xtcc` 不会替第三方应用重签名，也不会补齐 entitlement、描述文件、绑定 Team ID 的 app group 或钥匙串访问权限。替换或重签 provider 后，必须先由宿主重新激活扩展，再重新运行对应的创建命令刷新代码要求。
- **验证真实流量**：扩展的 `active`、配置的 `enabled` 以及隧道的 `connected` 仅反映注册与会话状态，无法证明 provider 已接管流量。使用前必须结合 provider 日志验证实际流量（DNS Proxy 必须同时验证 UDP 与 TCP 查询，VPN 必须验证目标路由收发包）。停用 VPN 且系统移除实时连接服务后，`ne status` 会报告服务不可用，而不会伪造 `disconnected`。
- **幂等与冲突保护**：重复批准和相同配置保存会复用原配置 UUID（系统明确返回 `NEConfigurationErrorDomain` 错误码 9 时输出 `unchanged: true`）。若同一 VPN provider 匹配多个已有配置，命令会拒绝写入。
- **超时与恢复**：单次客户端调用默认超时为 30 秒（可以通过 `ne --timeout` 设为 1 秒～300 秒）。超时后必须先检查 `ne list`（VPN 还必须检查 `ne status`）再决定是否重试；若 `connect` 超时后系统仍处于 `connecting`，可以执行 `ne disconnect <uuid>` 取消连接。完整测试场景参见 [NE 验证记录](VALIDATION.md#network-extension-validation)。

## 查阅命令参考

### 查阅顶层命令

| 命令 | 说明 |
| --- | --- |
| `build [--force]` | 调用 `make` 构建内嵌原生载荷的单文件 `xtcc`，要求原始源码目录仍然存在。 |
| `doctor` | 只读检查运行时工具路径与内嵌原生载荷，无需 `root` 权限。 |
| `start [minutes]` | 启动或续期 TCC 绕过会话（1 分钟～1440 分钟，默认 30 分钟）。别名：`enable`。 |
| `stop` | 停止 TCC 绕过，不卸载 `libxtcc_inject.dylib`。别名：`disable`。 |
| `status` | 查询已连接的 `tccd` 进程、绕过状态、到期时间与日志计数。 |
| `logs [--snapshot] [--all]` | 回放并持续读取 TCC 日志（`--snapshot` 单次读取；`--all` 包含 `com.apple.*`）。 |
| `ne [--timeout <seconds>] <subcommand>` | 管理 Network Extension（`--timeout` 范围为 1 秒～300 秒，默认 30 秒）。 |

### 查阅 NE 子命令

| 子命令 | 目标参数 | 说明 |
| --- | --- | --- |
| `ne list` | 无 | 只读列出已注册的 NE 系统扩展与已保存的网络配置。 |
| `ne approve <extension>` | 扩展 Bundle ID | 批准一个已注册的 NE 系统扩展。 |
| `ne filter <extension> [--vendor-configuration <plist>]` | 扩展 Bundle ID | 创建或更新并启用 Content Filter 配置。 |
| `ne dns-proxy <extension> [--vendor-configuration <plist>]` | 扩展 Bundle ID | 创建或更新并启用 DNS Proxy 配置。 |
| `ne vpn <extension> --server-address <addr> [--vendor-configuration <plist>]` | 扩展 Bundle ID | 创建或更新并启用 Packet Tunnel VPN 配置，不发起连接。 |
| `ne connect <uuid>` | 配置 UUID | 连接已启用的 VPN 并等待状态变为 `connected`。 |
| `ne disconnect <uuid>` | 配置 UUID | 断开 VPN 并等待状态变为 `disconnected`。 |
| `ne status <uuid>` | 配置 UUID | 通过 SystemConfiguration 读取 VPN 实时连接状态。 |
| `ne enable <uuid>` | 配置 UUID | 启用已保存的 `contentFilter`、`dnsProxy`、`VPN` 或 `appVPN` 配置。 |
| `ne disable <uuid>` | 配置 UUID | 停用指定配置，不卸载系统扩展。 |
| `ne remove <uuid>` | 配置 UUID | 删除指定配置，不卸载系统扩展。 |

全局参数 `--json` 用于输出紧凑的机器可读 JSON（`logs --json` 输出按行分隔的 NDJSON；`ne` 默认输出格式化 JSON，传入 `--json` 时输出单行紧凑 JSON）。输出内容绝不包含 vendor 配置字典或凭据。

## 构建与开发

### 满足构建要求

从源码构建与测试需满足以下条件：

- Apple Silicon Mac (`arm64` / `arm64e`)，最低部署目标为 macOS 15.0（实际运行兼容性取决于当前系统版本的私有框架接口）。
- Rust 1.85 及以上版本与 Cargo。
- Xcode（含 macOS SDK、Apple Clang 及支持 Python 的 LLDB）。
- 系统提供的 `make` 与 `codesign`。

### 执行构建与检查

在仓库根目录执行编译与离线测试：

```sh
make
make check
```

- **单文件构建**：`make` 执行 `cargo build --release --locked`，由 `build.rs` 编译并 ad-hoc 签名 `libxtcc_inject.dylib` 与 `libxtcc_ne.dylib`，最终内嵌生成单一可执行文件 `target/release/xtcc`。若 Cargo 不在默认路径 `$HOME/.cargo/bin/cargo`，可以执行 `make CARGO=/path/to/cargo`。
- **强制重新构建**：`make rebuild` 或 `xtcc build --force` 仅清理 `xtcc` 的 release 产物，然后重新构建可执行文件和两个原生库，保留依赖缓存与 debug 产物。
- **运行离线检查**：`make check` 运行格式检查、Clippy 与离线测试（覆盖协议编码、原生 ABI 布局、内嵌动态库释放、强制构建、Mach 端口所有权、broker 状态、日志缓冲与 CLI 输出）。构建回归测试使用隔离的临时产物目录。Mach 回环测试仅使用测试进程自建端口，不会附加 `tccd` 或修改系统权限。
- **持续集成检查**：[源码检查工作流](.github/workflows/source-checks.yml)负责检查 Rust 格式、Python 语法及除 `README_CN.md` 外的英文文本规范。

## 致谢与许可

感谢 [unixzii](https://github.com/unixzii "前往外部站点") 开发的 [tcc-bypass](https://github.com/unixzii/tcc-bypass "前往外部站点")。`xtcc` 的 TCC 控制层沿用了 `tcc-bypass` 的通信协议与生命周期设计，并自提交 `78da98104bafff7ecb96935cf198cb4196a28ffd` 保留了以下 Objective-C 文件：

- `native/tccd_inject.m`
- `native/Attribution.m`
- `native/Attribution.h`
- `native/ControlProtocol.h`

服务标识与日志子系统已改为 `dev.xtcc` 命名空间（`dev.xtcc.control` 与 `dev.xtcc`），注入库导出的 `cyan_tccd_*` 符号保持不变。

**许可说明**：上述上游提交未包含 `LICENSE` 文件。公开源码不代表授予再分发权利；在发布或分发包含上游派生代码的版本前，必须先取得原作者授权。`Cargo.toml` 已设置 `publish = false` 禁用包发布。
