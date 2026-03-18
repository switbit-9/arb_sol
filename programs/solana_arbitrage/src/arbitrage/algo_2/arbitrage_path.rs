use crate::arbitrage::base::Edge;
use std::mem::MaybeUninit;
use std::ops::Deref;

/// Fixed-capacity edge list (max 3). No heap allocation.
/// Derefs to `[Edge]` so `.iter()`, `.len()`, `[i]`, `.last()` all work.
#[derive(Clone)]
pub struct EdgeArray {
    buf: [MaybeUninit<Edge>; 3],
    len: u8,
}

impl EdgeArray {
    #[inline]
    pub fn from_2(e0: Edge, e1: Edge) -> Self {
        let mut buf: [MaybeUninit<Edge>; 3] = [MaybeUninit::uninit(), MaybeUninit::uninit(), MaybeUninit::uninit()];
        buf[0] = MaybeUninit::new(e0);
        buf[1] = MaybeUninit::new(e1);
        Self { buf, len: 2 }
    }

    #[inline]
    pub fn from_3(e0: Edge, e1: Edge, e2: Edge) -> Self {
        Self {
            buf: [MaybeUninit::new(e0), MaybeUninit::new(e1), MaybeUninit::new(e2)],
            len: 3,
        }
    }
}

impl Deref for EdgeArray {
    type Target = [Edge];
    #[inline]
    fn deref(&self) -> &[Edge] {
        unsafe { std::slice::from_raw_parts(self.buf.as_ptr() as *const Edge, self.len as usize) }
    }
}

/// Bridge for legacy code paths that still produce Vec<Edge>.
/// Panics if vec has more than 3 elements.
impl From<Vec<Edge>> for EdgeArray {
    fn from(v: Vec<Edge>) -> Self {
        let len = v.len() as u8;
        assert!(len <= 3, "EdgeArray supports at most 3 edges");
        let mut buf: [MaybeUninit<Edge>; 3] = [MaybeUninit::uninit(), MaybeUninit::uninit(), MaybeUninit::uninit()];
        for (i, e) in v.into_iter().enumerate() {
            buf[i] = MaybeUninit::new(e);
        }
        Self { buf, len }
    }
}

impl std::fmt::Debug for EdgeArray {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.deref().fmt(f)
    }
}

#[derive(Clone, Debug)]
pub struct ArbitragePath {
    pub edges: EdgeArray,
    pub profit: i128,
    pub final_amount: u128,
    pub start_amount: u64,
}
