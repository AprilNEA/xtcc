# 上游来源

本项目基于 unixzii/tcc-bypass 的设计与实现重写控制层。
上游地址：https://github.com/unixzii/tcc-bypass
参考提交：`78da98104bafff7ecb96935cf198cb4196a28ffd`。

以下 Objective-C 文件来自该提交：

- `native/tccd_inject.m`
- `native/Attribution.m`
- `native/Attribution.h`
- `native/ControlProtocol.h`

本轮将服务与日志标识改为 `dev.xtcc`。
注入端公开的 `cyan_tccd_*` 符号保持不变。
Rust 控制层参照了原协议和生命周期行为。

该提交中未发现 LICENSE 文件。
本目录不代表获得上游代码的再分发许可。
发布或分发前，请先向原作者确认授权。
`Cargo.toml` 设置了 `publish = false`。
