//! LCD 终端控制台 — ST7789 320×172 (适配 1.47inch)
//!
//! 将 kprintln! 输出镜像到 LCD 屏幕，支持自动换行和滚屏。
//! 当前专属字体: Tamzen 7x13 (通过 bitmap-font crate)

extern crate alloc;

use alloc::boxed::Box;
use bitmap_font::{TextStyle, tamzen::FONT_6x12};
use embedded_graphics::{
    Pixel,
    pixelcolor::{BinaryColor, Rgb565, raw::RawU16},
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::{Baseline, Text},
};
use embedded_graphics_framebuf::{FrameBuf, backends::FrameBufferBackend};
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_hal::{Blocking, delay::Delay, gpio::Output, spi::master::SpiDmaBus};
use mipidsi::{interface::SpiInterface, models::ST7789};

use mos::kernel::Task;

// ==========================================
// 字体与布局常量配置 (专属 Tamzen 7x13)
// ==========================================

// 字体宽高定义
const FONT_W: usize = 6;
const FONT_H: usize = 12;

// 屏幕物理分辨率设置
const LCD_WIDTH: usize = 320;
const LCD_HEIGHT: usize = 172; // 1.47寸屏幕物理高度
const LCD_BUFFER_SIZE: usize = LCD_WIDTH * LCD_HEIGHT;

// ST7789 显存偏移修正
// 由于 ST7789 内部通常是 240x320 的 GRAM，而 1.47 寸屏只引出了 172x320 的区域。
// 为了让图像居中显示，Y方向需要加上 (240 - 172) / 2 = 34 的偏移量。
const HARDWARE_Y_OFFSET: u16 = 34;
const HARDWARE_X_OFFSET: u16 = 0;

// 行高与边距配置
const ROW_H: usize = 14; // 行高略大于字体高度，留出 2px 的行间距
const MARGIN_LEFT: usize = 10; // 左侧留白
const MARGIN_TOP: usize = 10; // 顶部留白

// 根据屏幕尺寸和字体大小，计算出终端可以显示的列数和行数
const TERM_COLS: usize = (LCD_WIDTH - MARGIN_LEFT) / FONT_W;
const TERM_ROWS: usize = (LCD_HEIGHT - MARGIN_TOP) / ROW_H;

// 字符环形缓冲区大小，用于异步接收 kprintln! 发来的字符
const CHAR_BUF_SIZE: usize = 2048;

// 终端文本颜色
const COLOR_BLUE: Rgb565 = Rgb565::CSS_MEDIUM_AQUAMARINE;
const COLOR_GREEN: Rgb565 = Rgb565::CSS_PALE_GREEN; // 经典终端绿色
const COLOR_AMBER: Rgb565 = Rgb565::new(30, 40, 5); // 复古琥珀色

const COLOR_PURPLE: Rgb565 = Rgb565::CSS_DARK_SALMON;
const TERM_TEXT_COLOR: Rgb565 = COLOR_AMBER;

// ==========================================
// 颜色映射适配器 (BinaryColor -> Rgb565)
// ==========================================

/// 将单色字体 (BinaryColor) 映射为目标颜色 (Rgb565) 的绘制目标包装器。
/// 这样可以避免为每个字符绘制黑色背景，实现透明背景文字渲染。
struct BinaryToRgb<'a, D> {
    target: &'a mut D, // 底层真正的帧缓冲区
    color: Rgb565,     // 目标字体颜色
}

impl<'a, D: Dimensions> Dimensions for BinaryToRgb<'a, D> {
    fn bounding_box(&self) -> Rectangle {
        self.target.bounding_box()
    }
}

impl<'a, D: DrawTarget<Color = Rgb565>> DrawTarget for BinaryToRgb<'a, D> {
    type Color = BinaryColor;
    type Error = D::Error;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        // 拦截像素：如果是 "开" (文字线条)，就换成我们的终端颜色；否则丢弃(透明背景)
        self.target
            .draw_iter(pixels.into_iter().filter_map(|Pixel(p, c)| {
                if c == BinaryColor::On {
                    Some(Pixel(p, self.color))
                } else {
                    None
                }
            }))
    }
}

// ==========================================
// 帧缓冲区后端
// ==========================================

/// 分配在堆上的帧缓冲后端。
/// 172 * 320 * 2 bytes = ~110KB，如果在栈上分配极易导致内核/任务栈溢出，因此需要 Box 装载。
pub struct HeapBuffer<const N: usize>(pub Box<[u16; N]>);

impl<const N: usize> FrameBufferBackend for HeapBuffer<N> {
    type Color = Rgb565;

    #[inline(always)]
    fn set(&mut self, index: usize, color: Self::Color) {
        if index < N {
            // 将 Rgb565 颜色转换为底层的 u16 存储
            self.0[index] = RawU16::from(color).into_inner();
        }
    }

    #[inline(always)]
    fn get(&self, index: usize) -> Self::Color {
        if index < N {
            Rgb565::from(RawU16::new(self.0[index]))
        } else {
            Rgb565::BLACK
        }
    }

    fn nr_elements(&self) -> usize {
        N
    }
}

// ==========================================
// 字符环形缓冲区
// ==========================================

/// 一个简单的单生产者单消费者 (SPSC) 环形缓冲区，用于缓存待打印的字符。
struct CharRing {
    buf: [u8; CHAR_BUF_SIZE],
    head: usize, // 读取位置
    tail: usize, // 写入位置
    len: usize,  // 当前积压的字符数量
}

impl CharRing {
    const fn new() -> Self {
        Self {
            buf: [0; CHAR_BUF_SIZE],
            head: 0,
            tail: 0,
            len: 0,
        }
    }

    /// 向缓冲区压入字符。如果在中断或多个任务中调用，需要外部加锁保护。
    fn push(&mut self, c: u8) {
        if self.len < CHAR_BUF_SIZE {
            self.buf[self.tail] = c;
            self.tail = (self.tail + 1) % CHAR_BUF_SIZE;
            self.len += 1;
        }
        // 如果满了就直接丢弃新字符，不阻塞当前任务（保证 print 不会导致系统卡死）
    }

    /// 批量取出数据到 out 切片中，返回实际取出的字节数。
    fn drain(&mut self, out: &mut [u8]) -> usize {
        let n = self.len.min(out.len());
        for i in 0..n {
            out[i] = self.buf[self.head];
            self.head = (self.head + 1) % CHAR_BUF_SIZE;
        }
        self.len -= n;
        n
    }
}

// 全局静态的字符缓冲区
static mut CHAR_RING: CharRing = CharRing::new();

/// 向 LCD 字符缓冲区推送一个字节 (通常由 kputchar / panic handler 底层调用)
#[inline]
pub fn lcd_putchar(c: u8) {
    // 使用 critical_section 保证 push 操作的原子性和中断安全性
    critical_section::with(|_| unsafe {
        CHAR_RING.push(c);
    });
}

// ==========================================
// Display 类型 & 全局存储
// ==========================================

// 定义基于 DMA 的 SPI 设备类型和 LCD 驱动接口类型
type LcdSpi = SpiDmaBus<'static, Blocking>;
type LcdDevice = ExclusiveDevice<LcdSpi, Output<'static>, Delay>;
type LcdInterface = SpiInterface<'static, LcdDevice, Output<'static>>;
pub type LcdDisplay = mipidsi::Display<LcdInterface, ST7789, Output<'static>>;

// 全局保存 LCD 的实例和背光控制引脚
static mut LCD_DISPLAY: Option<LcdDisplay> = None;
static mut LCD_BACKLIGHT: Option<Output<'static>> = None;

/// main 初始化阶段调用: 将已初始化的 display 存入全局 static
///
/// # Safety
/// 必须在 lcd_task 启动前调用，且整个生命周期内只能调用一次，以防数据竞争。
pub unsafe fn store_display(display: LcdDisplay, backlight: Output<'static>) {
    LCD_DISPLAY = Some(display);
    LCD_BACKLIGHT = Some(backlight);
}

// ==========================================
// Terminal 状态机 (带脏行追踪)
// ==========================================

/// 终端状态机，负责维护文字矩阵、光标位置，并决定哪些区域需要刷新。
struct Terminal {
    /// 屏幕字符矩阵 (二维数组，仅包含当前可见区域的内容)
    screen: [[u8; TERM_COLS]; TERM_ROWS],
    /// 每一行实际有内容的字符数（用于优化渲染，无需渲染行尾的空白）
    lens: [u8; TERM_ROWS],
    /// 当前光标所在的行索引
    cursor_row: usize,
    /// 当前光标所在的列索引
    cursor_col: usize,
    /// 脏行标记: 记录哪些行发生了文字改变，需要重新推送到 LCD
    dirty: [bool; TERM_ROWS],
}

impl Terminal {
    fn new() -> Self {
        Self {
            screen: [[b' '; TERM_COLS]; TERM_ROWS],
            lens: [0; TERM_ROWS],
            cursor_row: 0,
            cursor_col: 0,
            dirty: [false; TERM_ROWS],
        }
    }

    /// 馈入一个字符并解析控制符
    fn feed(&mut self, c: u8) {
        match c {
            b'\n' => self.newline(),            // 换行
            b'\r' => self.cursor_col = 0,       // 回车：光标归零
            0x08 | 0x7F => self.backspace(),    // 退格符或 Delete 键
            c if c >= 0x20 => self.put_char(c), // 可打印字符 (ASCII >= 32)
            _ => {}                             // 忽略其他不可打印的控制字符
        }
    }

    /// 写入常规可打印字符
    fn put_char(&mut self, c: u8) {
        // 如果到达行尾，自动换行
        if self.cursor_col >= TERM_COLS {
            self.newline();
        }
        self.screen[self.cursor_row][self.cursor_col] = c;
        self.cursor_col += 1;

        // 更新这一行的有效长度
        if self.cursor_col > self.lens[self.cursor_row] as usize {
            self.lens[self.cursor_row] = self.cursor_col as u8;
        }

        // 标记当前行为“脏”，需要重绘
        self.dirty[self.cursor_row] = true;
    }

    /// 处理换行逻辑
    fn newline(&mut self) {
        self.cursor_col = 0;
        if self.cursor_row < TERM_ROWS - 1 {
            // 光标未到最后一行，直接下移
            self.cursor_row += 1;
        } else {
            // 光标已在最后一行，触发向上滚屏
            self.scroll_up();
        }
        // 换行后新的一行被触及，标记为脏
        self.dirty[self.cursor_row] = true;
    }

    /// 处理退格逻辑
    fn backspace(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
            // 将退格位置视为空格清空
            self.screen[self.cursor_row][self.cursor_col] = b' ';

            // 如果是在行尾退格，缩短有效长度
            if self.cursor_col < self.lens[self.cursor_row] as usize {
                self.lens[self.cursor_row] = self.cursor_col as u8;
            }
            self.dirty[self.cursor_row] = true;
        }
    }

    /// 屏幕内容整体向上滚动一行
    fn scroll_up(&mut self) {
        for i in 0..TERM_ROWS - 1 {
            self.screen[i] = self.screen[i + 1];
            self.lens[i] = self.lens[i + 1];
        }
        // 清空最后一行
        self.screen[TERM_ROWS - 1] = [b' '; TERM_COLS];
        self.lens[TERM_ROWS - 1] = 0;

        // 滚屏会导致所有行的内容都发生变化，因此所有行全标记为脏
        self.dirty = [true; TERM_ROWS];
    }

    /// 计算当前有多少行是被修改过的
    fn dirty_count(&self) -> usize {
        self.dirty.iter().filter(|&&d| d).count()
    }

    /// 清除所有的脏标记 (通常在 SPI 刷屏完成后调用)
    fn clear_dirty(&mut self) {
        self.dirty = [false; TERM_ROWS];
    }

    /// 将终端的特定行渲染到 FrameBuffer (内存操作)
    fn render_row(&self, row: usize, fb: &mut FrameBuf<Rgb565, HeapBuffer<LCD_BUFFER_SIZE>>) {
        // 使用 ROW_H 计算该行在屏幕上的 Y 坐标基准点
        let y = MARGIN_TOP as i32 + row as i32 * ROW_H as i32;

        // 1. 擦除背景：使用黑色矩形覆盖这一行的整条区域
        Rectangle::new(Point::new(0, y), Size::new(LCD_WIDTH as u32, ROW_H as u32))
            .into_styled(PrimitiveStyle::with_fill(Rgb565::BLACK))
            .draw(fb)
            .unwrap();

        let len = self.lens[row] as usize;
        if len > 0 {
            // 计算文字的垂直居中 Y 坐标 (基于行高和字高)
            let text_y = y + ((ROW_H - FONT_H) / 2) as i32;

            if let Ok(line) = core::str::from_utf8(&self.screen[row][..len]) {
                // 设置字体，前景激活
                let style = TextStyle::new(&FONT_6x12, BinaryColor::On);

                // 套上颜色映射包装器，将 Binary 转换为终端 Rgb 色
                let mut mapped_fb = BinaryToRgb {
                    target: fb,
                    color: TERM_TEXT_COLOR,
                };

                // 绘制文本字符串
                Text::with_baseline(
                    line,
                    Point::new(MARGIN_LEFT as i32, text_y),
                    style,
                    Baseline::Top,
                )
                .draw(&mut mapped_fb)
                .ok();
            }
        }
    }

    /// 全量渲染：将终端所有的可见字符一次性绘制到 FrameBuffer 中
    fn render_full(&self, fb: &mut FrameBuf<Rgb565, HeapBuffer<LCD_BUFFER_SIZE>>) {
        // 全局清屏为黑色
        fb.clear(Rgb565::BLACK).unwrap();

        let style = TextStyle::new(&FONT_6x12, BinaryColor::On);

        let mut mapped_fb = BinaryToRgb {
            target: fb,
            color: TERM_TEXT_COLOR,
        };

        // 遍历所有有内容的行
        for row in 0..TERM_ROWS {
            let len = self.lens[row] as usize;
            if len == 0 {
                continue;
            }
            let y = row as i32 * ROW_H as i32 + MARGIN_TOP as i32;
            let text_y = y + ((ROW_H - FONT_H) / 2) as i32;

            if let Ok(line) = core::str::from_utf8(&self.screen[row][..len]) {
                Text::with_baseline(
                    line,
                    Point::new(MARGIN_LEFT as i32, text_y),
                    style,
                    Baseline::Top,
                )
                .draw(&mut mapped_fb)
                .ok();
            }
        }
    }
}

// ==========================================
// SPI 刷新辅助函数 (推送到物理屏幕)
// ==========================================

/// 将整个 FrameBuffer 全局推送到 SPI 显示器
fn flush_full(display: &mut LcdDisplay, fb: &FrameBuf<Rgb565, HeapBuffer<LCD_BUFFER_SIZE>>) {
    display
        .set_pixels(
            HARDWARE_X_OFFSET,
            HARDWARE_Y_OFFSET,
            LCD_WIDTH as u16 - 1 + HARDWARE_X_OFFSET,
            LCD_HEIGHT as u16 - 1 + HARDWARE_Y_OFFSET,
            // 提取原生数据并将 u16 转回 Rgb565
            fb.data.0.iter().map(|&p| Rgb565::from(RawU16::new(p))),
        )
        .ok();
}

/// 局部刷新：只将特定行的对应内存区域推送到 SPI 显示器。
/// 极大节省了 SPI 传输时间，对提高终端响应速度十分有效。
fn flush_row(
    display: &mut LcdDisplay,
    fb: &FrameBuf<Rgb565, HeapBuffer<LCD_BUFFER_SIZE>>,
    row: usize,
) {
    // 计算该行在 FrameBuffer 中的 Y 坐标范围
    let y_start_fb = (MARGIN_TOP + row * ROW_H) as u16;
    let y_end_fb = y_start_fb + ROW_H as u16 - 1;

    // 防止越界
    if y_end_fb >= LCD_HEIGHT as u16 {
        return;
    }

    // 计算映射到物理屏幕上的实际 Y 坐标 (加上硬件偏移)
    let y_start_lcd = y_start_fb + HARDWARE_Y_OFFSET;
    let y_end_lcd = y_end_fb + HARDWARE_Y_OFFSET;

    // 计算这一行切片在一维数组中的起点和终点索引
    let start_idx = y_start_fb as usize * LCD_WIDTH;
    let end_idx = (y_end_fb as usize + 1) * LCD_WIDTH;

    if end_idx <= LCD_BUFFER_SIZE {
        display
            .set_pixels(
                HARDWARE_X_OFFSET,
                y_start_lcd,
                LCD_WIDTH as u16 - 1 + HARDWARE_X_OFFSET,
                y_end_lcd,
                fb.data.0[start_idx..end_idx]
                    .iter()
                    .map(|&p| Rgb565::from(RawU16::new(p))),
            )
            .ok();
    }
}

// ==========================================
// LCD 渲染任务
// ==========================================

/// 在堆上分配全零的内存，并返回 Box<T>。
/// 使用 alloc_zeroed 可以防止在分配巨型数组(如FrameBuffer)时，栈先被撑爆然后再拷贝到堆的问题。
unsafe fn box_zeroed<T>() -> Box<T> {
    let layout = core::alloc::Layout::new::<T>();
    let ptr = alloc::alloc::alloc_zeroed(layout) as *mut T;
    if ptr.is_null() {
        alloc::alloc::handle_alloc_error(layout);
    }
    Box::from_raw(ptr)
}

/// LCD 后台守护任务：负责从字符缓冲区读取数据，更新状态机并刷新物理屏幕。
/// 通常应该作为一个独立的 RTOS Task 或异步协程运行。
pub fn lcd_task() {
    let display = unsafe { LCD_DISPLAY.as_mut().expect("LCD display not stored") };

    // 点亮背光
    unsafe {
        if let Some(ref mut bl) = LCD_BACKLIGHT {
            bl.set_high();
        }
    }

    // 初始化帧缓冲区和终端状态机（都在堆上分配以节省栈空间）
    let fb_data: Box<[u16; LCD_BUFFER_SIZE]> = unsafe { box_zeroed() };
    let mut fb = FrameBuf::new(HeapBuffer(fb_data), LCD_WIDTH, LCD_HEIGHT);
    let mut term: Box<Terminal> = unsafe { box_zeroed() };

    // 初始化时用空格填满矩阵
    for row in term.screen.iter_mut() {
        row.fill(b' ');
    }

    // 首次全量清屏
    fb.clear(Rgb565::BLACK).unwrap();
    flush_full(display, &fb);

    // 用于每次批量从环形缓冲区中取出的缓冲片
    let mut drain_buf = [0u8; 128];

    // 主渲染循环
    loop {
        // 在临界区内安全地取出积压的字符
        let n = critical_section::with(|_| unsafe { CHAR_RING.drain(&mut drain_buf) });

        if n > 0 {
            // 将读取到的所有字符喂给终端状态机
            for &c in &drain_buf[..n] {
                term.feed(c);
            }

            let dirty_count = term.dirty_count();

            // 智能刷新策略：
            // 如果大量行都变脏了 (比如发生了滚屏，所有行都脏了)，那么逐行刷新会发起太多次短距 SPI 传输，开销反而大。
            // 此时直接一次性全量绘制并全量推送。
            if dirty_count > TERM_ROWS / 2 {
                // 大量脏行 (如滚屏) → 全屏渲染 + 全屏刷新
                term.render_full(&mut fb);
                flush_full(display, &fb);
            } else {
                // 如果只是少量的行变脏 (比如用户正常打字)，则仅重新渲染这几行，并且只通过 SPI 传送这几行对应的内存带。
                for row in 0..TERM_ROWS {
                    if term.dirty[row] {
                        term.render_row(row, &mut fb);
                        flush_row(display, &fb, row);
                    }
                }
            }

            // 本轮渲染完成，清空脏标记
            term.clear_dirty();
            // 短暂休眠交出控制权
            Task::delay(10);
        } else {
            // 队列为空时，休眠稍长一点以节省 CPU 资源
            Task::delay(50);
        }
    }
}
