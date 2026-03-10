//! 缓冲区 (对应 mos-renode/core/kernel/data_type/buffer.hpp)
//!
//! SyncRxBuf 已移至板级 crate (依赖 kernel::sync::Sema)

/// 固定大小的线性缓冲区
pub struct Buffer<T: Copy, const N: usize> {
    raw: [T; N],
    len: usize,
}

impl<T: Copy, const N: usize> Buffer<T, N> {
    pub const fn new() -> Self {
        unsafe {
            Self {
                raw: core::mem::MaybeUninit::zeroed().assume_init(),
                len: 0,
            }
        }
    }

    #[inline]
    pub const fn max_size() -> usize { N }

    #[inline]
    pub fn full(&self) -> bool { self.len >= N }

    #[inline]
    pub fn empty(&self) -> bool { self.len == 0 }

    #[inline]
    pub fn len(&self) -> usize { self.len }

    pub fn push(&mut self, data: T) {
        if !self.full() {
            self.raw[self.len] = data;
            self.len += 1;
        }
    }

    pub fn back(&self) -> Option<&T> {
        if self.empty() { None } else { Some(&self.raw[self.len - 1]) }
    }

    pub fn pop(&mut self) {
        if !self.empty() { self.len -= 1; }
    }

    pub fn clear(&mut self) { self.len = 0; }

    #[inline]
    pub fn as_slice(&self) -> &[T] { &self.raw[..self.len] }

    pub fn iter(&self) -> core::slice::Iter<'_, T> { self.raw[..self.len].iter() }
}

pub type RxBuf<const N: usize> = Buffer<u8, N>;