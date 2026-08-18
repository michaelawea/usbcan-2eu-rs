# usbcan-2eu

用 Rust 写的 **ZLG USBCAN-E-U / USBCAN-2E-U** macOS 用户态驱动。

厂商提供 Windows 和 Linux 驱动，但没有 macOS 版本。本项目通过 Apple 的 IOUSBHost
框架直接跟设备走 USB 通信，不需要装内核扩展、不需要链接厂商库、不需要代码签名。

[English](README.md)

---

> **非官方项目。** 本项目与广州致远电子（ZLG）无任何隶属、授权、背书或支持关系。
> "ZLG"、"USBCAN" 是其各自所有者的商标，此处仅用于说明本驱动适用的硬件。项目不包含、
> 也不分发任何厂商的软件、驱动、固件或文档。使用者需自行拥有相应硬件。

---

## 快速开始

```bash
git clone https://github.com/michaelawea/usbcan-2eu-rs
cd usbcan-2eu-rs
cargo build --release

# 设备在不在，长什么样
./target/release/usbcan2eu info

# 看总线
./target/release/usbcan2eu dump --bitrate 250k

# 发一帧
./target/release/usbcan2eu send 123#DEADBEEF --bitrate 250k
```

需要 macOS 11 以上、Rust 1.75 以上。Apple Silicon 和 Intel 都可以。不需要 `sudo`、
不需要装驱动、不需要重启。

## 它到底能不能用

把通道 0 和通道 1 对接起来（CANH–CANH、CANL–CANL，两端各一个 120 Ω 终端电阻），
然后直接问它：

```bash
usbcan2eu selftest --bitrate 500k
```

```text
  adapter          USBCAN-2E-U at index 0
  both channels    started at 500 kbit/s

  can0 -> can1     100/100 frames, 0 errors
  can1 -> can0     100/100 frames, 0 errors

PASS
```

这条命令把整条链路都跑了一遍——位定时、包封装、发送应答、接收解析——全部在真实
硬件上验证，不需要任何第三方设备。报 bug 之前请先跑这个。

## 接入其他工具

### SLCAN：给 python-can 等一切工具用

macOS 没有 SocketCAN，大部分 CAN 工具看不到这个设备。桥接层把它变成一个 SLCAN
串口设备：

```bash
usbcan2eu slcan --bitrate 500k
# SLCAN bridge for channel 0 is up.
#   device   /dev/ttys004
```

```python
import can

bus = can.Bus(interface="slcan", channel="/dev/ttys004", bitrate=500000)
for msg in bus:
    print(msg)
```

任何能通过串口说 SLCAN 的工具都同理：`cantools`、`SavvyCAN`、你自己的脚本。

### embedded-can

打开 `embedded-can` feature 后，`CanFrame` 实现 `embedded_can::Frame`，
`CanChannel` 实现 `embedded_can::blocking::Can`，按通用 trait 写的代码可以直接跑：

```toml
[dependencies]
usbcan-2eu = { version = "0.1", default-features = false, features = ["embedded-can"] }
```

### 当库用

```toml
[dependencies]
usbcan-2eu = { version = "0.1", default-features = false }
```

```rust,no_run
use usbcan_2eu::{Bitrate, CanFrame, ChannelMode, Device, Error};

let device = Device::open_first()?;
device.start_channel(0, Bitrate::Kbps500, ChannelMode::Normal)?;

device.transmit(0, &[CanFrame::new(0x123, false, &[0xde, 0xad])])?;

loop {
    match device.receive(200) {
        Ok(chunk) => chunk.frames_on(0).for_each(|f| println!("{f}")),
        Err(Error::Timeout { .. }) => continue,   // 总线空闲，不是错误
        Err(e) => return Err(e),
    }
}
# Ok::<(), Error>(())
```

`cargo run --example receive` / `transmit` / `loopback` / `list_devices` 是上面
这些用法的完整可跑版本。

### Feature 一览

| Feature | 默认 | 作用 |
|---|---|---|
| `cli` | 是 | `usbcan2eu` 命令行工具。只当库用时关掉，避免引入 `clap`。 |
| `slcan` | 随 `cli` | SLCAN 桥接。 |
| `embedded-can` | 否 | `embedded_can` trait 实现。 |
| `serde` | 否 | 公开数据类型的 `Serialize`。 |

## 硬件支持

| 型号 | Product ID | 通道数 | 状态 |
|---|---|---|---|
| USBCAN-2E-U | `0x1261` | 2 | 真机验证通过 |
| USBCAN-E-U | `0x1260` | 1 | 理论可用，**从未测试，欢迎反馈** |

逆向得到的协议会随固件版本漂移。`usbcan2eu info` 会打印一个识别值，提 issue 时请附上。

如果你手上有 USBCAN-E-U，跑一下 `usbcan2eu info` 然后开个 issue 贴上输出——不管
成功还是失败——都是很有价值的贡献。见 [`docs/hardware-testing.md`](docs/hardware-testing.md)。

支持的波特率：10、20、50、100、125、250、500、800、1000 kbit/s。每一档都是精确值
而非近似，见 [`docs/protocol.md`](docs/protocol.md#bit-timing)。

## 协议是怎么搞清楚的

这个设备用的是没有公开文档的 USB 协议。本实现通过研究厂商的 Windows 驱动栈得到
线索，然后在真实设备上逐条验证——包格式、校验和、命令时序、位定时，全部由观察硬件
的实际行为确认。

文档描述的是**设备可观测的行为**，这既是实现所依赖的东西，也是任何人拿一台 USB
分析仪就能独立复核的东西。本仓库不含任何厂商代码、二进制、头文件或反编译产物，
源码中也没有从中拷贝过任何内容。

凡是推断而非实测得出的结论，代码和文档里都会明说。控制器时钟频率是影响最大的一处，
[`docs/protocol.md`](docs/protocol.md#bit-timing) 里说明了它是怎么确定下来的。

## 文档

| 文档 | 内容 |
|---|---|
| [`docs/protocol.md`](docs/protocol.md) | 线上协议规格，给想用别的语言重新实现的人 |
| [`docs/macos-usb-isoc-kernel-bug.md`](docs/macos-usb-isoc-kernel-bug.md) | 为什么本驱动不用 libusb，以及背后那个 macOS 内核 bug |
| [`docs/hardware-testing.md`](docs/hardware-testing.md) | 需要接设备的测试怎么跑 |
| [`docs/troubleshooting.md`](docs/troubleshooting.md) | 症状 → 原因对照表 |
| [API 文档](https://docs.rs/usbcan-2eu) | 由源码生成 |

## 安全提示

这个工具会往真实 CAN 总线上发帧。在车辆、储能系统和工业设备上，CAN 报文会让东西
真的动起来、切换状态。

- 不要在行驶中的车辆上使用。
- 不要接到你不能承受其被干扰的设备上。
- `--listen-only` 会让通道不主动驱动总线，但这一点尚未用分析仪确认为真正的电气静默。

本软件不提供任何形式的担保，详见许可证。

## 参与贡献

欢迎 issue 和 PR，尤其欢迎 USBCAN-2E-U 以外型号的设备反馈。见
[`CONTRIBUTING.md`](CONTRIBUTING.md)——包括没有硬件时怎么参与。

## 许可证

本项目在以下两个许可证下双授权：

- Apache License 2.0（[`LICENSE-APACHE`](LICENSE-APACHE)）
- MIT（[`LICENSE-MIT`](LICENSE-MIT)）

由你任选其一。除非另行声明，你有意提交的任何贡献都按上述方式双授权，不附加其他条款。
