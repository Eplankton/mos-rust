//! 交互式 Shell — BLE 输入 + LCD 输出

use mos::config::*;
use mos::kernel::Task;
use mos::kernel::global::{TCBS, USER_NAME, os_ticks};
use core::fmt;

use crate::board_info::*;

// ==========================================
// LCD-only 输出
// ==========================================

struct LcdWriter;

impl fmt::Write for LcdWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            crate::lcd::lcd_putchar(b);
        }
        Ok(())
    }
}

#[doc(hidden)]
pub fn _lcd_print(args: fmt::Arguments) {
    use fmt::Write;
    LcdWriter.write_fmt(args).ok();
}

/// LCD-only 打印 (不换行)
#[macro_export]
macro_rules! lprint {
    ($($arg:tt)*) => {
        $crate::shell::_lcd_print(::core::format_args!($($arg)*))
    };
}

/// LCD-only 打印 (换行)
#[macro_export]
macro_rules! lprintln {
    () => {
        $crate::shell::_lcd_print(::core::format_args!("\n"))
    };
    ($($arg:tt)*) => {{
        $crate::shell::_lcd_print(::core::format_args!($($arg)*));
        $crate::shell::_lcd_print(::core::format_args!("\n"));
    }};
}

// ==========================================
// BLE 输入队列
// ==========================================

const INPUT_BUF_SIZE: usize = 256;

struct InputRing {
    buf: [u8; INPUT_BUF_SIZE],
    head: usize,
    tail: usize,
    len: usize,
}

impl InputRing {
    const fn new() -> Self {
        Self {
            buf: [0; INPUT_BUF_SIZE],
            head: 0,
            tail: 0,
            len: 0,
        }
    }

    fn push(&mut self, c: u8) {
        if self.len < INPUT_BUF_SIZE {
            self.buf[self.tail] = c;
            self.tail = (self.tail + 1) % INPUT_BUF_SIZE;
            self.len += 1;
        }
    }

    fn pop(&mut self) -> Option<u8> {
        if self.len == 0 {
            return None;
        }
        let c = self.buf[self.head];
        self.head = (self.head + 1) % INPUT_BUF_SIZE;
        self.len -= 1;
        Some(c)
    }
}

static mut SHELL_INPUT_RING: InputRing = InputRing::new();

/// BLE write callback 调用
pub fn shell_push_input(data: &[u8]) {
    critical_section::with(|_| unsafe {
        for &b in data {
            SHELL_INPUT_RING.push(b);
        }
        SHELL_INPUT_RING.push(b'\n');
    });
}

fn shell_getchar_nonblock() -> Option<u8> {
    critical_section::with(|_| unsafe { SHELL_INPUT_RING.pop() })
}

// ==========================================
// 命令类型
// ==========================================

#[derive(Clone, Copy)]
struct Cmd {
    text: &'static str,
    callback: fn(&str),
}

impl Cmd {
    fn try_match<'a>(&self, input: &'a str) -> Option<&'a str> {
        let input = input.trim_start();
        let len = self.text.len();
        if input.starts_with(self.text) {
            let rest = &input[len..];
            if rest.is_empty() || rest.starts_with(' ') {
                return Some(rest.trim_start());
            }
        }
        None
    }
}

static SYS_CMD_MAP: &[Cmd] = &[
    Cmd { text: "ls",     callback: ls_cmd },
    Cmd { text: "kill",   callback: kill_cmd },
    Cmd { text: "block",  callback: block_cmd },
    Cmd { text: "resume", callback: resume_cmd },
    Cmd { text: "help",   callback: help_cmd },
    Cmd { text: "time",   callback: time_cmd },
    Cmd { text: "uname",  callback: uname_cmd },
    Cmd { text: "reboot", callback: reboot_cmd },
];

fn task_ctrl_cmd(argv: &str, ok: impl FnOnce(usize), err: fn()) {
    let name = argv.trim();
    if name.is_empty() {
        err();
    } else if let Some(tid) = Task::find_tid(name) {
        ok(tid);
    } else {
        lprintln!("[MOS] Unknown task '{}'", name);
    }
}

fn bad_argv() {
    lprintln!("[MOS] Invalid Arguments");
}

fn parse_and_run(line: &str) {
    lprintln!("> {}", line);
    if line.trim().is_empty() {
        return;
    }
    for cmd in SYS_CMD_MAP {
        if let Some(argv) = cmd.try_match(line) {
            (cmd.callback)(argv);
            return;
        }
    }
    lprintln!("[MOS] Unknown command '{}'", line.trim());
}

// ==========================================
// 内置命令
// ==========================================

fn ls_cmd(_argv: &str) {
    lprintln!(
        "{:>3} {:<10} {:>3} {:>8} {:>5}",
        "ID", "Name", "PRI", "Status", "Stack"
    );
    lprintln!("{}", "--- ---------- --- -------- -----");
    unsafe {
        for tid in 0..TASK_MAX {
            let tcb = &TCBS[tid];
            if tcb.status == mos::kernel::tcb::TaskStatus::Terminated {
                continue;
            }
            let stack_top = tcb
                .stack_bottom
                .saturating_add((PAGE_SIZE_WORDS as u32) * 4);
            let used_words = stack_top.saturating_sub(tcb.sp) / 4;
            let pct = (used_words * 100 / PAGE_SIZE_WORDS as u32).min(100);
            lprintln!(
                "{:>3} {:<10} {:>3} {:>8} {:>4}%",
                tid, tcb.name, tcb.priority, tcb.status, pct
            );
        }
    }
    lprintln!("----------------------------------");
}

fn kill_cmd(argv: &str) {
    task_ctrl_cmd(
        argv,
        |tid| {
            lprintln!("[MOS] Task '{}' terminated", unsafe { TCBS[tid].name });
            Task::terminate(tid);
        },
        bad_argv,
    );
}

fn block_cmd(argv: &str) {
    task_ctrl_cmd(
        argv,
        |tid| {
            lprintln!("[MOS] Task '{}' blocked", unsafe { TCBS[tid].name });
            Task::block(tid);
        },
        bad_argv,
    );
}

fn resume_cmd(argv: &str) {
    task_ctrl_cmd(
        argv,
        |tid| {
            lprintln!("[MOS] Task '{}' resumed", unsafe { TCBS[tid].name });
            Task::resume(tid);
        },
        bad_argv,
    );
}

fn help_cmd(_argv: &str) {
    for cmd in SYS_CMD_MAP {
        lprint!("{}, ", cmd.text);
    }
    lprintln!();
}

fn time_cmd(_argv: &str) {
    let up_secs = os_ticks() / SYSTICK_HZ;
    let h = up_secs / 3600;
    let m = (up_secs % 3600) / 60;
    let s = up_secs % 60;
    lprintln!("========= Uptime: {:02}:{:02}:{:02} =========", h, m, s);
}

fn uname_cmd(argv: &str) {
    let new_name = argv.trim();
    if !new_name.is_empty() {
        unsafe {
            let bytes = new_name.as_bytes();
            let copy_len = bytes.len().min(USER_NAME_SIZE - 1);
            USER_NAME[..copy_len].copy_from_slice(&bytes[..copy_len]);
            USER_NAME[copy_len..].fill(0);
            lprintln!("[MOS] User Name => {}", new_name);
        }
    }
    let name_str = unsafe {
        core::str::from_utf8(&USER_NAME)
            .unwrap_or(MOS_USER_NAME)
            .trim_end_matches('\0')
    };
    lprintln!(" A_A       _  [{}] @ {}", name_str, MOS_VERSION);
    lprintln!("o'' )_____//  Arch  @ {}", MOS_ARCH);
    lprintln!(" `_/  MOS  )  Chip  @ {}", MOS_MCU);
    lprintln!(" (_(_/--(_/   2023-2026 Copyright by Eplankton");
}

fn reboot_cmd(_argv: &str) {
    lprintln!("[MOS] Reboot!");
    mos::arch::reboot();
}

// ==========================================
// Shell 主循环
// ==========================================

struct LineEditor {
    buf: [u8; SHELL_BUF_SIZE],
    len: usize,
}

impl LineEditor {
    const fn new() -> Self {
        Self {
            buf: [0u8; SHELL_BUF_SIZE],
            len: 0,
        }
    }

    fn feed(&mut self, c: u8) {
        match c {
            b'\r' | b'\n' => {
                if let Ok(line) = core::str::from_utf8(&self.buf[..self.len]) {
                    parse_and_run(line.trim());
                }
                self.len = 0;
            }
            0x08 | 0x7F => {
                if self.len > 0 {
                    self.len -= 1;
                }
            }
            c if c >= 0x20 && self.len < SHELL_BUF_SIZE - 1 => {
                self.buf[self.len] = c;
                self.len += 1;
            }
            _ => {}
        }
    }
}

/// Shell 任务入口
pub fn launch() {
    let mut editor = LineEditor::new();
    uname_cmd("");
    ls_cmd("");
    loop {
        match shell_getchar_nonblock() {
            Some(c) => editor.feed(c),
            None => Task::delay(10),
        }
    }
}
