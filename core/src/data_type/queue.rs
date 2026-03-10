//! 环形队列 (对应 mos-renode/core/kernel/data_type/queue.hpp)

use core::mem::MaybeUninit;

/// 固定大小环形队列
pub struct Queue<T, const N: usize> {
    data: [MaybeUninit<T>; N],
    head: usize,
    tail: usize,
    len: usize,
}

impl<T: Copy, const N: usize> Queue<T, N> {
    pub const fn new() -> Self {
        Self {
            data: [const { MaybeUninit::uninit() }; N],
            head: 0,
            tail: 0,
            len: 0,
        }
    }

    #[inline]
    pub const fn capacity() -> usize { N }

    #[inline]
    pub fn size(&self) -> usize { self.len }

    #[inline]
    pub fn full(&self) -> bool { self.len >= N }

    #[inline]
    pub fn empty(&self) -> bool { self.len == 0 }

    #[inline]
    pub fn clear(&mut self) {
        self.head = 0;
        self.tail = 0;
        self.len = 0;
    }

    #[inline]
    pub fn front(&self) -> Option<&T> {
        if self.empty() { None }
        else { unsafe { Some(self.data[self.head].assume_init_ref()) } }
    }

    #[inline]
    pub fn back(&self) -> Option<&T> {
        if self.empty() { None }
        else {
            let idx = if self.tail == 0 { N - 1 } else { self.tail - 1 };
            unsafe { Some(self.data[idx].assume_init_ref()) }
        }
    }

    pub fn push(&mut self, val: T) -> bool {
        if self.full() { return false; }
        self.data[self.tail] = MaybeUninit::new(val);
        self.tail = (self.tail + 1) % N;
        self.len += 1;
        true
    }

    pub fn pop(&mut self) -> bool {
        if self.empty() { return false; }
        self.head = (self.head + 1) % N;
        self.len -= 1;
        true
    }

    pub fn serve(&mut self) -> Option<T> {
        if self.empty() { return None; }
        let val = unsafe { self.data[self.head].assume_init_read() };
        self.head = (self.head + 1) % N;
        self.len -= 1;
        Some(val)
    }

    pub fn iter(&self, mut f: impl FnMut(&T)) {
        if self.empty() { return; }
        let mut i = self.head;
        for _ in 0..self.len {
            unsafe { f(self.data[i].assume_init_ref()) };
            i = (i + 1) % N;
        }
    }
}