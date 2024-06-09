use std::arch::x86_64::_mm_pause;
use std::slice::SliceIndex;
use std::cell::UnsafeCell;
use std::sync::atomic::{compiler_fence, fence, AtomicUsize, Ordering};
#[inline]
#[cold]
fn cold() {}

#[inline]
fn likely(b: bool) -> bool {
    if !b { cold() }
    b
}

#[inline]
fn unlikely(b: bool) -> bool {
    if b { cold() }
    b
}

#[derive(Default)]
#[repr(align(64))]
pub struct SeqLock<T> {
    version: AtomicUsize,
    data: UnsafeCell<T>,
}
unsafe impl<T: Send> Send for SeqLock<T> {}
unsafe impl<T: Sync> Sync for SeqLock<T> {}

impl<T: Copy> SeqLock<T> {
    pub fn new(data: T) -> Self {
        Self {version: AtomicUsize::new(0), data: UnsafeCell::new(data)}
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
    use super::*;
    use std::{sync::atomic::AtomicBool, time::{Duration, Instant}};

    fn read_test<const N: usize>()
    {
        let lock = SeqLock::new([0usize; N]);
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
        read_test::<{2usize.pow(16)}>()
    }
}



