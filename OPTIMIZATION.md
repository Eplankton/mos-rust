# MOS-RS 进阶优化备忘

以下优化尚未实施，按风险/收益排序供后续参考。

---

## 1. Sleeping List 最小堆 (性能, 风险: 中)

**文件**: `core/src/kernel/sched.rs` (`wake_up_sleepers`)

**现状**: `wake_up_sleepers()` 每个 tick 线性遍历整个 `SLEEPING_LIST` (O(N))。

**方案**: 改用 min-heap 按 `wake_point` 排序，只需比较堆顶即可判断是否有到期任务。`async_rt.rs` 的 `TimerQueue` 已有完整的 min-heap 实现（sift_up/sift_down），可直接复用。

**改动点**:
- 新增 `SleepHeap` 结构（复用 `TimerQueue` 逻辑）
- 替换 `SLEEPING_LIST` 全局变量
- 修改 `Task::delay()`、`Sema::down_timeout()`、`MsgQueue::wait_on()` 中的插入逻辑

---

## 2. TaskList 位图调度 (性能, 风险: 中)

**文件**: `core/src/kernel/list.rs`

**现状**: `TaskList` 用 `heapless::Vec<usize, 16>` 实现，`insert_in_order` / `remove` 均为 O(N)。

**方案**: 使用 `u16` bitmap（每个 bit 对应一个 tid），配合 TCBS 优先级查找：
```rust
struct TaskBitmap {
    bits: u16,  // bit i = tid i 在队列中
}

impl TaskBitmap {
    fn insert(&mut self, tid: usize) { self.bits |= 1 << tid; }
    fn remove(&mut self, tid: usize) { self.bits &= !(1 << tid); }
    fn highest_priority(&self) -> Option<usize> {
        // 遍历置位的 tid，找 TCBS[tid].priority 最小者
    }
}
```

**改动点**: 所有使用 `TaskList` 的地方（READY_LIST、BLOCKED_LIST、sync/ipc 等待队列）

---

## 3. LCD 行缓冲替代全屏 FrameBuffer (RAM, 风险: 中)

**文件**: `boards/esp32c6/src/lcd.rs`

**现状**: 全屏 framebuffer `320×172×2 = ~110KB` 常驻堆内存。

**方案**: 改用行缓冲 `320×14×2 ≈ 9KB`，每次只渲染一行并通过 SPI 发送：
- 局部更新（最常见）：性能不变，逐行渲染+发送
- 全屏刷新（滚屏）：需要 TERM_ROWS 次 SPI 传输（当前一次），但节省 ~100KB 堆内存
- `render_row()` 改为渲染到行缓冲后立即 `flush_row()`

**权衡**: 滚屏时多次短 SPI 传输 vs 节省 100KB RAM

---

## 4. 字模直接 blit 替代 embedded-graphics (ROM + 性能, 风险: 低)

**文件**: `boards/esp32c6/src/lcd.rs` (`render_row` / `render_full`)

**现状**: `Text::draw()` → `BinaryToRgb` → `FrameBuf` 逐像素迭代，多层泛型膨胀。

**方案**: 直接从 bitmap-font 的字模数据按行写入 u16 数组：
```rust
fn blit_char(fb: &mut [u16], x: usize, y: usize, ch: u8, color: u16) {
    let glyph = FONT_6x12.glyph(ch);
    for row in 0..FONT_H {
        for col in 0..FONT_W {
            if glyph.pixel(col, row) {
                fb[(y + row) * LCD_WIDTH + x + col] = color;
            }
        }
    }
}
```

**收益**: 节省 ~2-5KB ROM（消除 embedded-graphics 泛型实例化），渲染速度提升 2-3x。
**代价**: 失去 embedded-graphics 生态兼容性，需自行处理字模数据提取。

---

## 5. ecall 小帧上下文切换 (性能, 风险: 高)

**文件**: `core/src/arch/riscv.rs` (`_mos_trap_handler`)

**现状**: trap handler 对所有 trap（ecall + timer 中断）都保存全部 30 个 GP 寄存器（128 字节）。

**方案**: 区分两种路径：
- **ecall（主动 yield）**: 只保存 callee-saved 寄存器 `{ra, s0-s11, mepc, mstatus}` ≈ 60 字节
- **timer 中断（抢占）**: 保存全部 30 个寄存器（128 字节）

```
ecall 路径:
  addi sp, sp, -60
  sw ra, 0(sp)
  sw s0-s11, 4..48(sp)
  sw mepc, 52(sp)
  sw mstatus, 56(sp)
  ...

timer 路径:
  addi sp, sp, -128
  sw x1..x31 (完整保存)
  ...
```

**关键难点**:
- `next_tcb()` 恢复时需要知道当前帧是大帧还是小帧（可在帧中存标志位，或用 sp 对齐判断）
- 任务可能在 ecall 中挂起，被 timer 中断唤醒时需要按小帧恢复
- WiFi blob 产生的异常也需要正确分类

**收益**: ecall 上下文切换延迟减少约 40%（少保存/恢复 16 个寄存器）。


这段对话记录展示了一个典型的嵌入式系统（基于 Rust 和 RISC-V，可能是 ESP32-C6）的内核开发和调试过程。其中涉及的**优化手段**可以归纳为以下几个维度：

---

### 1. 内存与栈优化 (Memory & Stack Optimization)

这是对话中篇幅最大的部分，主要通过监控和调整来确保系统稳定性：

* **水位线监控 (`stack_watermark`)**：通过向栈空间填充特定字节（`STACK_WATERMARK`），从底部向上搜索非填充值来计算实际使用的栈深度。这为开发者提供了量化的依据，用于判断某个任务是否面临溢出风险。
* **动态与静态栈分配的权衡**：
* 将 `Shell` 任务从 `Task::create`（静态池）改为 `Task::create_raw`（堆分配），并手动指定更大的栈空间（2048B）。
* **NOTE 提示**：明确了水位线函数暂不支持堆任务，体现了对工具局限性的认知。


* **二进制体积优化**：尝试了 `opt-level = "z"`（极致体积优化）与 `"s"`（性能/体积平衡）的对比。通过实测发现 "z" 反而导致 WiFi 库相关代码膨胀，最终**回退到更优的 "s" 级别**。

### 2. 内核同步机制优化 (Kernel Sync Optimization)

针对内核调度和同步原语（Semaphore, Mutex）的逻辑修复：

* **清理资源泄露（SLEEPING_LIST）**：在 `Sema::up()` 中增加了对 `SLEEPING_LIST` 的主动清理。
* **逻辑优化**：如果一个任务是因为信号量释放被唤醒的，必须清除它在 `down_timeout()` 中注册的定时器条目。这防止了任务同时存在于“就绪队列”和“睡眠队列”导致的调度混乱。


* **状态一致性**：强制将 `wake_point` 置 0 或特定值，确保 TCB（任务控制块）状态的原子性和一致性。

### 3. 构建与兼容性优化 (Build & Feature Gating)

* **功能门控 (Conditional Compilation)**：引入 `#[cfg(feature = "async")]`。
* 通过 Cargo Features（`default = ["async"]`）让内核变得模块化。如果不使用异步运行时，可以减小二进制体积并加快编译速度。


* **移除冗余编译选项**：去掉了 `.cargo/config.toml` 中的 `force-frame-pointers`。在某些 RISC-V 环境下，减少帧指针（Frame Pointer）的维护可以节省寄存器资源并略微提升性能。

### 4. 驱动与交互优化 (Driver & I/O Optimization)

* **非阻塞环形缓冲区 (Ring Buffers)**：
* `SHELL_INPUT_RING` (256B) 和 `CHAR_RING` (2048B) 的设计实现了外设（LCD/串口）与任务逻辑的解耦。
* **策略选择**：`CHAR_RING` 在满载时选择“直接丢弃”而不是阻塞，这在实时系统中能有效防止由于显示任务慢导致的业务逻辑挂死。


* **任务优先级编排**：
* 明确了 BT（蓝牙）、BLE、LCD 和 Shell 的优先级顺序（从 4 到 1）。将高实时性要求的 WiFi/蓝牙任务置于高优先级，保证通讯不掉线。



---

### 5. 调试方法论优化 (Debugging Methodology)

虽然不是代码优化，但这是解决问题的优化手段：

* **控制变量实验**：当增加 Shell 栈空间后 Bug 反而更容易触发时，及时**否定了“栈溢出是唯一原因”的假设**，将重心转向同步机制（Sema/Mutex）的漏洞分析。
* **工具选型**：针对 `defmt` 的讨论，评估了其在 LCD 实时显示场景下的可行性，最终基于“目标端可读性”保留了传统的格式化输出。

---