//! 位图数据结构 (对应 mos-renode/core/kernel/data_type/bitmap.hpp)

/// 创建 BitMap 类型别名的辅助宏
#[macro_export]
macro_rules! bitmap_type {
    ($n:expr) => {
        $crate::data_type::bitmap::BitMap<$n, { ($n + 31) / 32 }>
    };
}

/// 固定大小位图
pub struct BitMap<const N: usize, const W: usize> {
    data: [u32; W],
}

impl<const N: usize, const W: usize> BitMap<N, W> {
    pub const fn new() -> Self {
        Self { data: [0u32; W] }
    }

    #[inline]
    pub fn set(&mut self, pos: usize) {
        if pos < N {
            let index = pos / 32;
            let bit = 31 - (pos % 32);
            self.data[index] |= 1 << bit;
        }
    }

    #[inline]
    pub fn reset(&mut self, pos: usize) {
        if pos < N {
            let index = pos / 32;
            let bit = 31 - (pos % 32);
            self.data[index] &= !(1 << bit);
        }
    }

    #[inline]
    pub fn test(&self, pos: usize) -> bool {
        if pos < N {
            let index = pos / 32;
            let bit = 31 - (pos % 32);
            (self.data[index] & (1 << bit)) != 0
        } else {
            false
        }
    }

    pub fn first_zero(&self) -> Option<usize> {
        for i in 0..W {
            if self.data[i] != !0u32 {
                let bit = (!self.data[i]).leading_zeros() as usize;
                let pos = i * 32 + bit;
                if pos < N {
                    return Some(pos);
                }
            }
        }
        None
    }

    pub fn first_one(&self) -> Option<usize> {
        for i in 0..W {
            if self.data[i] != 0 {
                let bit = self.data[i].leading_zeros() as usize;
                let pos = i * 32 + bit;
                if pos < N {
                    return Some(pos);
                }
            }
        }
        None
    }

    pub fn count_ones(&self) -> usize {
        let mut total = 0;
        for i in 0..W {
            total += self.data[i].count_ones() as usize;
        }
        total
    }

    #[inline]
    pub fn count_zeros(&self) -> usize {
        N - self.count_ones()
    }
}