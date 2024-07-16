use std::{
    arch::x86_64::_mm_pause,
    cell::UnsafeCell,
    slice::SliceIndex,
    sync::atomic::{compiler_fence, fence, AtomicUsize, Ordering},
};
#[inline]
#[cold]
fn cold() {}

#[inline]
fn likely(b: bool) -> bool {
    if !b {
        cold()
    }
    b
}

#[inline]
fn unlikely(b: bool) -> bool {
    if b {
        cold()
    }
    b
}

#[repr(align(64))]
pub struct Seqlock<T> {
    version: AtomicUsize,
    _pad: [u8; 56],
    data:    UnsafeCell<T>,
}
impl<T: Default> Default for Seqlock<T> {
    fn default() -> Self {
        Self { version: Default::default(),  _pad: [0; 56], data: Default::default() }
    }
}
unsafe impl<T: Send> Send for Seqlock<T> {}
unsafe impl<T: Sync> Sync for Seqlock<T> {}

impl<T: Copy> Seqlock<T> {
    pub fn new(data: T) -> Self {
        Self { version: Default::default(), _pad: [0; 56], data: UnsafeCell::new(data) }
    }

    #[inline(never)]
    pub fn read(&self, result: &mut T) {
        loop {
            let v1 = self.version.load(Ordering::Acquire);
            compiler_fence(Ordering::AcqRel);
            *result = unsafe { *self.data.get() };
            compiler_fence(Ordering::AcqRel);
            let v2 = self.version.load(Ordering::Acquire);
            if v1 == v2 && v1 & 1 == 0 {
                return;
            }
        }
    }

    #[inline(never)]
    pub fn pessimistic_read(&self, result: &mut T) {
        loop {
            let v1 = self.version.load(Ordering::Acquire);
            if v1 & 1 == 1 {
                continue;
            }

            compiler_fence(Ordering::AcqRel);
            *result = unsafe { *self.data.get() };
            compiler_fence(Ordering::AcqRel);
            let v2 = self.version.load(Ordering::Acquire);
            if v1 == v2 {
                return;
            }
        }
    }

    #[inline(never)]
    pub fn write_old(&self, val: &T) {
        let v = self.version.load(Ordering::Relaxed).wrapping_add(1);
        self.version.store(v, Ordering::Relaxed);
        unsafe { *self.data.get() = *val };
        self.version.store(v.wrapping_add(1), Ordering::Relaxed);
    }

    #[inline(never)]
    pub fn write(&self, val: &T) {
        let v = self.version.fetch_add(1, Ordering::Release);
        compiler_fence(Ordering::AcqRel);
        unsafe { *self.data.get() = *val };
        compiler_fence(Ordering::AcqRel);
        self.version.store(v.wrapping_add(2), Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::AtomicBool,
        time::{Duration, Instant},
    };

    use super::*;

    fn read_test<const N: usize>() {
        let lock = Seqlock::new([0usize; N]);
        let done = AtomicBool::new(false);
        std::thread::scope(|s| {
            s.spawn(|| {
                 let mut msg = [0usize; N];
                 while !done.load(Ordering::Relaxed) {
                     lock.read(&mut msg);
                     let first = msg[0];
                     for i in msg {
                         assert_eq!(first, i); // data consistency is verified here
                     }
                 }
                 assert_ne!(msg[0], 0)
             });
            s.spawn(|| {
                 let curt = Instant::now();
                 let mut count = 0;
                 let mut msg = [0usize; N];
                 while curt.elapsed() < Duration::from_secs(1) {
                     msg.fill(count);
                     lock.write(&msg);
                     count = count.wrapping_add(1);
                 }
                 done.store(true, Ordering::Relaxed);
             });
        });
    }

    #[test]
    fn read_16() {
        read_test::<16>()
    }
    #[test]
    fn read_32() {
        read_test::<32>()
    }
    #[test]
    fn read_64() {
        read_test::<64>()
    }
    #[test]
    fn read_128() {
        read_test::<128>()
    }
    #[test]
    fn read_large() {
        read_test::<{ 2usize.pow(16) }>()
    }
}
