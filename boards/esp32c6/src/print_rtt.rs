//! RTT 日志后端 — 板级实现
//!
//! 初始化 RTT 通道并注册到 mos 的打印抽象层。

use core::fmt;

static mut RTT_CHANNEL: Option<rtt_target::UpChannel> = None;
static mut RTT_DOWN_CHANNEL: Option<rtt_target::DownChannel> = None;

/// 初始化 RTT 并注册到 mos
pub fn init() {
    let channels = rtt_target::rtt_init! {
        up: {
            0: {
                size: 1024
            }
        }
        down: {
            0: {
                size: 64
            }
        }
    };
    unsafe {
        RTT_CHANNEL = Some(channels.up.0);
        RTT_DOWN_CHANNEL = Some(channels.down.0);
    }

    // 注册到 mos 打印抽象层
    mos::print::register_print_fn(rtt_print);
    mos::print::register_getchar_fn(rtt_getchar);
}

/// 向 RTT 输出格式化文本
fn rtt_print(args: fmt::Arguments) {
    use fmt::Write;
    struct RttWriter;
    impl fmt::Write for RttWriter {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            unsafe {
                if let Some(ref mut ch) = RTT_CHANNEL {
                    ch.write(s.as_bytes());
                }
            }
            Ok(())
        }
    }
    RttWriter.write_fmt(args).ok();
}

/// 非阻塞读取 RTT 输入
fn rtt_getchar() -> Option<u8> {
    unsafe {
        if let Some(ref mut down) = RTT_DOWN_CHANNEL {
            let mut buf = [0u8; 1];
            if down.read(&mut buf) > 0 {
                return Some(buf[0]);
            }
        }
    }
    None
}

/// 向 LCD 和 RTT 同时输出单个字节 (供 lcd 模块调用)
#[inline]
pub fn kputchar(c: u8) {
    unsafe {
        if let Some(ref mut ch) = RTT_CHANNEL {
            ch.write(&[c]);
        }
    }
}
