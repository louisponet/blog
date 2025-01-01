use std::{alloc::Layout,  mem::size_of, sync::atomic::{AtomicUsize, Ordering}};

use thiserror::Error;
use crate::seqlock::{ReadError, Seqlock};

#[derive(Error, Debug)]
pub enum QueueError {
    #[error("Queue not initialized")]
    UnInitialized,
    #[error("Queue length not power of two")]
    LengthNotPowerOfTwo,
    #[error("Element size not power of two - 4")]
    ElementSizeNotPowerTwo,
    #[cfg(feature = "shmem")]
    #[error("Shmem error")]
    SharedMemoryError(#[from] shared_memory::ShmemError),
}

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum QueueType {
    Unknown,
    MPMC,
    SPMC,
}

#[derive(Debug)]
#[repr(C)]
pub struct QueueHeader {
    pub queue_type:         QueueType,   // 1
    pub is_initialized:     u8,          // 2
    _pad1:                  [u8; 2],          // 4
    pub elsize:             u32,         // 8
    mask:                   usize,       // 16
    pub count:              AtomicUsize, // 24
}
impl QueueHeader {
    /// in bytes
    pub fn sizeof(&self) -> usize {
        (self.mask + 1) * (self.elsize as usize)
    }

    pub fn n_elements(&self) -> usize {
        self.mask + 1
    }

    pub fn from_ptr(ptr: *mut u8) -> &'static mut Self {
        unsafe { &mut *(ptr as *mut Self) }
    }
}

#[cfg(feature = "shmem")]
impl QueueHeader {
    pub fn shared<P: AsRef<std::path::Path>>(path: P) -> &'static mut Self {
        use shared_memory::ShmemConf;
        match ShmemConf::new()
            .flink(&path)
            .open()
        {
            Ok(shmem) => {
                let o = unsafe { &mut *(shmem.as_ptr() as *mut QueueHeader)};
                std::mem::forget(shmem);
                o
            }
            _ => panic!("couldn't open shmem")
        }
    }
}


fn power_of_two(mut v: usize) -> usize {
    let mut n = 0;
    while v % 2 == 0 {
        v /= 2;
        n += 1;
    }
    n
}

#[repr(C, align(64))]
pub struct Queue<T> {
    pub header: QueueHeader,
    buffer:     [Seqlock<T>],
}

impl<T: Copy> Queue<T> {
    /// Allocs (unshared) memory and initializes a new queue from it
    pub fn new(len: usize, queue_type: QueueType) -> Result<&'static Self, QueueError> {
        let real_len = len.next_power_of_two();
        let size =
            std::mem::size_of::<QueueHeader>() + real_len * std::mem::size_of::<Seqlock<T>>();

        unsafe {
            let ptr = std::alloc::alloc_zeroed(
                Layout::array::<u8>(size)
                    .unwrap()
                    .align_to(64)
                    .unwrap()
                    .pad_to_align(),
            );
            // Why real len you may ask. The size of the fat pointer ONLY includes the length of the
            // unsized part of the struct i.e. the buffer.
            Self::from_uninitialized_ptr(ptr, real_len, queue_type)
        }
    }

    pub const fn size_of(len: usize) -> usize {
        size_of::<QueueHeader>()
            + len.next_power_of_two() * size_of::<Seqlock<T>>()
    }

    pub fn from_uninitialized_ptr(
        ptr: *mut u8,
        len: usize,
        queue_type: QueueType,
    ) -> Result<&'static Self, QueueError> {
        if !len.is_power_of_two() {
            return Err(QueueError::LengthNotPowerOfTwo);
        }
        unsafe {
            let q = &mut *(std::ptr::slice_from_raw_parts_mut(ptr, len) as *mut Queue<T>);
            let elsize = size_of::<Seqlock<T>>();
            if !len.is_power_of_two() {
                return Err(QueueError::LengthNotPowerOfTwo);
            }

            let mask = len - 1;

            q.header.queue_type = queue_type;
            q.header.mask = mask;
            q.header.elsize = elsize as u32;
            q.header.is_initialized = true as u8;
            q.header.count = AtomicUsize::new(0);
            Ok(q)
        }
    }

    #[allow(dead_code)]
    pub fn from_initialized_ptr(ptr: *mut QueueHeader) -> Result<&'static Self, QueueError> {
        unsafe {
            let len = (*ptr).mask + 1;
            if !len.is_power_of_two() {
                return Err(QueueError::LengthNotPowerOfTwo);
            }
            if (*ptr).is_initialized != true as u8 {
                return Err(QueueError::UnInitialized);
            }

            Ok(&*(std::ptr::slice_from_raw_parts_mut(ptr, len) as *const Queue<T>))
        }
    }

    // Note: Calling this from anywhere that's not a producer -> false sharing
    pub fn count(&self) -> usize {
        self.header.count.load(Ordering::Relaxed)
    }

    fn next_count(&self) -> usize {
        match self.header.queue_type {
            QueueType::Unknown => panic!("Unknown queue"),
            QueueType::MPMC => self.header.count.fetch_add(1, Ordering::AcqRel),
            QueueType::SPMC => {
                let c = self.header.count.load(Ordering::Relaxed);
                self.header
                    .count
                    .store(c.wrapping_add(1), Ordering::Relaxed);
                c
            }
        }
    }

    fn load(&self, pos: usize) -> &Seqlock<T> {
        unsafe { self.buffer.get_unchecked(pos) }
    }

    fn version(&self) -> usize {
        ((self.count() / (self.header.mask + 1)) << 1) + 2
    }

    // returns the current count
    fn produce(&self, item: &T) -> usize {
        let p = self.next_count();
        let lock = self.load(p & self.header.mask);
        lock.write(item);
        p
    }

    fn consume(&self, el: &mut T, ri: usize, ri_ver: usize) -> Result<(), ReadError> {
        self.load(ri).read_with_version(el, ri_ver)
    }

    fn len(&self) -> usize {
        self.header.mask + 1
    }

}

unsafe impl<T> Send for Queue<T> {}
unsafe impl<T> Sync for Queue<T> {}

impl<T: std::fmt::Debug> std::fmt::Debug for Queue<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Queue:\nHeader:\n{:?}", self.header)
    }
}

#[cfg(feature = "shmem")]
impl<T: Copy> Queue<T> {
    pub fn shared<P: AsRef<std::path::Path>>(
        shmem_flink: P,
        size: usize,
        typ: QueueType,
    ) -> Result<&'static Self, QueueError> {
        use shared_memory::{ShmemConf, ShmemError};
        match ShmemConf::new()
            .size(Self::size_of(size))
            .flink(&shmem_flink)
            .create()
        {
            Ok(shmem) => {
                let ptr = shmem.as_ptr();
                std::mem::forget(shmem);
                Self::from_uninitialized_ptr(ptr, size, typ)
            }
            Err(ShmemError::LinkExists) => {
                let shmem = ShmemConf::new().flink(shmem_flink).open().unwrap();
                let ptr = shmem.as_ptr() as *mut QueueHeader;
                std::mem::forget(shmem);
                Self::from_initialized_ptr(ptr)
            }
            Err(e) => {
                eprintln!(
                    "Unable to create or open shmem flink {:?} : {e}",
                    shmem_flink.as_ref()
                );
                Err(e.into())
            }
        }
    }

    pub fn open_shared<P: AsRef<std::path::Path>>(
        shmem_flink: P,
    ) -> Result<&'static Self, QueueError> {
        use shared_memory::ShmemConf;
        match ShmemConf::new()
            .flink(&shmem_flink)
            .open()
        {
            Ok(shmem) => {
                let ptr = shmem.as_ptr() as *mut QueueHeader;
                std::mem::forget(shmem);
                unsafe {
                    Self::shared(shmem_flink, (*ptr).n_elements(), (*ptr).queue_type)
                }
            }
            Err(e) => {
                eprintln!(
                    "Unable to create or open shmem flink {:?} : {e}",
                    shmem_flink.as_ref()
                );
                Err(e.into())
            }
        }
    }
}

/// Simply exists for the automatic produce_first
#[repr(C, align(64))]
pub struct Producer<'a, T> {
    pub queue:      &'a Queue<T>,
}

impl<'a, T: Copy> From<&'a Queue<T>> for Producer<'a, T> {
    fn from(queue: &'a Queue<T>) -> Self {
        Self {
            queue,
        }
    }
}

impl<'a, T: Copy> Producer<'a, T> {
    pub fn produce(&mut self, msg: &T) -> usize {
        self.queue.produce(msg)
    }
}

impl<'a, T> AsMut<Producer<'a, T>> for Producer<'a, T> {
    fn as_mut(&mut self) -> &mut Producer<'a, T> {
        self
    }
}

#[repr(C)]
#[derive(Debug)]
pub struct Consumer<'a, T> {
    /// Shared reference to the channel
    /// Read index pointer
    pos:              usize, // 8
    mask:             usize,        // 16
    expected_version: usize,        // 24
    queue:            &'a Queue<T>, // 48 fat ptr: (usize, pointer)
}

impl<'a, T: Copy> Consumer<'a, T> {
    fn update_pos(&mut self) {
        self.pos = (self.pos + 1) & self.mask;
        self.expected_version += 2 * (self.pos == 0) as usize;
    }

    /// Nonblocking consume returning either Ok(()) or a ReadError
    pub fn try_consume(&mut self, el: &mut T) -> Result<(), ReadError> {
        self.queue.consume(el, self.pos, self.expected_version)?;
        self.update_pos();
        Ok(())
    }

}

impl<'a, T> AsMut<Consumer<'a, T>> for Consumer<'a, T> {
    fn as_mut(&mut self) -> &mut Consumer<'a, T> {
        self
    }
}

impl<'a, T: Copy> From<&'a Queue<T>> for Consumer<'a, T> {
    fn from(queue: &'a Queue<T>) -> Self {
        let c = queue.header.count.load(Ordering::Relaxed);
        let pos = c & queue.header.mask;
        let expected_version = ((c / queue.len()) << 1) + 2;
        Self {
            pos,
            mask: queue.header.mask,
            expected_version,
            queue,
        }
    }
}

#[cfg(test)]
mod test {
    use crate::seqlock::ReadError;

    use super::*;

    #[test]
    fn power_of_two_test() {
        let t = 128;
        assert_eq!(power_of_two(t), 7);
    }
    #[test]
    fn headersize() {
        assert_eq!(std::mem::size_of::<QueueHeader>(), 24);
        assert_eq!(40, std::mem::size_of::<Consumer<'_, [u8; 60]>>())
    }

    #[test]
    fn basic() {
        for typ in [QueueType::SPMC, QueueType::MPMC] {
            let q = Queue::new(16, typ).unwrap();
            let mut p = Producer::from(q);
            let mut c = Consumer::from(q);
            p.produce(&1);
            let mut m = 0;

            assert_eq!(c.try_consume(&mut m), Ok(()));
            assert_eq!(m, 1);
            assert!(matches!(c.try_consume(&mut m), Err(ReadError::Empty)));
            for i in 0..16 {
                p.produce(&i);
            }
            for i in 0..16 {
                c.try_consume(&mut m).unwrap();
                assert_eq!(m, i);
            }

            assert!(matches!(c.try_consume(&mut m), Err(ReadError::Empty)));

            for i in 0..20 {
                p.produce(&1);
            }

            assert!(matches!(c.try_consume(&mut m), Err(ReadError::SpedPast)));
        }
    }

    fn multithread(n_writers: usize, n_readers: usize, tot_messages: usize) {
        let q = Queue::new(16, QueueType::MPMC).unwrap();

        let mut readhandles = Vec::new();
        for n in 0..n_readers {
            let mut c1 = Consumer::from(q);
            let cons = std::thread::spawn(move || {
                let mut c = 0;
                let mut m = 0;
                while c < tot_messages {
                    while c1.try_consume(&mut m).is_err() {
                    }
                    c += m;
                }
                assert_eq!(c, (0..tot_messages).sum::<usize>());
            });
            readhandles.push(cons)
        }
        let mut writehandles = Vec::new();
        for n in 0..n_writers {
            let mut p1 = Producer::from(q);
            let prod1 = std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(20));
                let mut c = n;
                while c < tot_messages {
                    p1.produce(&c);
                    c += n_writers;
                    std::thread::yield_now();
                }
            });
            writehandles.push(prod1);
        }

        for h in readhandles {
            h.join();
        }
        for h in writehandles {
            h.join();
        }
    }
    #[test]
    fn multithread_1_2() {
        multithread(1, 2, 100000);
    }
    #[test]
    fn multithread_1_4() {
        multithread(1, 4, 100000);
    }
    #[test]
    fn multithread_2_4() {
        multithread(2, 4, 100000);
    }
    #[test]
    fn multithread_4_4() {
        multithread(4, 4, 100000);
    }
    #[test]
    fn multithread_8_8() {
        multithread(8, 8, 100000);
    }
    #[test]
    #[cfg(feature = "shmem")]
    fn basic_shared() {
        for typ in [QueueType::SPMC, QueueType::MPMC] {
            let path = std::path::Path::new("/dev/shm/blabla_test");
            std::fs::remove_file(path);
            let q = Queue::shared(path, 16, typ).unwrap();
            let mut p = Producer::from(q);
            let mut c = Consumer::from(q);

            p.produce(&1);
            let mut m = 0;

            assert_eq!(c.try_consume(&mut m), Ok(()));
            assert_eq!(m, 1);
            assert!(matches!(c.try_consume(&mut m), Err(ReadError::Empty)));
            for i in 0..16 {
                p.produce(&i);
            }
            for i in 0..16 {
                c.try_consume(&mut m).unwrap();
                assert_eq!(m, i);
            }

            assert!(matches!(c.try_consume(&mut m), Err(ReadError::Empty)));

            for i in 0..20 {
                p.produce(&1);
            }

            assert!(matches!(c.try_consume(&mut m), Err(ReadError::SpedPast)));
            std::fs::remove_file(&path);
        }
    }
}
