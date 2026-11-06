mod non_zero;

#[allow(unused)]
pub use non_zero::*;

// Used by linux_x11 only; the other platforms report it as dead.
#[allow(dead_code)]
#[inline]
pub fn is_aligned<T>(ptr: *const T) -> bool {
    let align = std::mem::align_of::<T>();
    (ptr as usize) & (align - 1) == 0
}

