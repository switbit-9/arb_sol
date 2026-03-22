use crate::programs::ProgramInstance;
use crate::MAX_POOLS;

pub struct PoolVec {
    buf: Box<[std::mem::MaybeUninit<ProgramInstance>; MAX_POOLS]>,
    len: usize,
}

impl Clone for PoolVec {
    fn clone(&self) -> Self {
        let mut new = PoolVec::new();
        for item in self.iter() {
            new.push(item.clone());
        }
        new
    }
}

impl PoolVec {
    pub fn new() -> Self {
        Self {
            buf: Box::new([const { std::mem::MaybeUninit::uninit() }; MAX_POOLS]),
            len: 0,
        }
    }

    #[inline]
    pub fn push(&mut self, val: ProgramInstance) {
        self.buf[self.len] = std::mem::MaybeUninit::new(val);
        self.len += 1;
    }
}

impl std::ops::Deref for PoolVec {
    type Target = [ProgramInstance];
    fn deref(&self) -> &[ProgramInstance] {
        unsafe { std::slice::from_raw_parts(self.buf.as_ptr().cast(), self.len) }
    }
}

impl std::ops::DerefMut for PoolVec {
    fn deref_mut(&mut self) -> &mut [ProgramInstance] {
        unsafe { std::slice::from_raw_parts_mut(self.buf.as_mut_ptr().cast(), self.len) }
    }
}

impl Drop for PoolVec {
    fn drop(&mut self) {
        for i in 0..self.len {
            unsafe { self.buf[i].assume_init_drop(); }
        }
    }
}
