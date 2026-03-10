//! 任务队列与链表管理 (对应 mos-renode/core/kernel/data_type/list.hpp)

use heapless::Vec;
use crate::config::TASK_MAX;

/// 安全的任务列表
pub struct TaskList {
    tids: Vec<usize, TASK_MAX>,
}

impl TaskList {
    pub const fn new() -> Self {
        Self { tids: Vec::new() }
    }

    #[inline]
    pub fn push_back(&mut self, tid: usize) -> Result<(), usize> {
        self.tids.push(tid)
    }

    #[inline]
    pub fn remove(&mut self, tid: usize) {
        if let Some(pos) = self.tids.iter().position(|&x| x == tid) {
            self.tids.remove(pos);
        }
    }

    #[inline]
    pub fn pop_front(&mut self) -> Option<usize> {
        if self.tids.is_empty() { None }
        else { Some(self.tids.remove(0)) }
    }

    #[inline]
    pub fn front(&self) -> Option<usize> {
        self.tids.first().copied()
    }

    #[inline]
    pub fn front_ref(&self) -> Option<&usize> {
        self.tids.first()
    }

    #[inline]
    pub fn is_empty(&self) -> bool { self.tids.is_empty() }

    #[inline]
    pub fn len(&self) -> usize { self.tids.len() }

    pub fn insert_in_order<F>(&mut self, tid: usize, mut is_before: F) -> Result<(), usize>
    where
        F: FnMut(usize) -> bool,
    {
        if self.tids.is_full() { return Err(tid); }
        let mut insert_pos = self.tids.len();
        for (i, &current_tid) in self.tids.iter().enumerate() {
            if is_before(current_tid) {
                insert_pos = i;
                break;
            }
        }
        let _ = self.tids.insert(insert_pos, tid);
        Ok(())
    }

    #[inline]
    pub fn iter(&self) -> core::slice::Iter<'_, usize> {
        self.tids.iter()
    }
}