use std::arch::x86_64::__rdtscp;
use std::sync::atomic::{AtomicI32, Ordering};

use code::Seqlock;
// use ma_queues::seqlock::SeqLock;
use core_affinity::CoreId;
use ma_time::{Instant, Nanos};
use ma_timing::Timer;
use rand::Rng;

impl Default for TimingMessage {
    fn default() -> Self {
        Self {
            rdtscp: Instant::default(),
            data:   [0; 1],
        }
    }
}
fn rdtscp() -> u64 {
    unsafe { __rdtscp(&mut 0u32 as *mut _) }
}

// New observations.
// # benchmark, not realistic maybe
// baseline: writing 1 consumer -> 21-37 nanos
//           reading 1 consumer -> 45-50 nanos
// padding with [u8; 56]: writing 1 consumer -> 60 - 78 ns
//                        reading 1 consumer -> 92-110 ns
// + pessimistic_reading: writing 1 consumer -> 17(!) - 44(?) ns
//                      : reading 1 consumer -> 100 - 210ns
//
// # actual latency
// baseline: writing 1 consumer -> 21-37 nanos
//           reading 1 consumer -> 45-50 nanos
// padding with [u8; 56]: writing 1 consumer -> 28 - 35ns
//                        reading 1 consumer -> 60-62 ns
// + pessimistic_reading: writing 1 consumer -> 28-35 ns
//                      : reading 1 consumer -> 41ns(all stars aligned, 1/10) 60-64ns (9/10) avg/med
//
//  no fetch_add + baseline    + no_padding: 93 ns
//  no fetch_add + pessimistic + no_padding: 97 ns
//  no fetch_add + pessimistic + padding:    90 ns


// false sharing 128 bit?
//

// const N: usize = 1;
// #[inline(never)]
// fn reader<const N: usize>(done: &AtomicBool, lock: &SeqLock<[usize;N]>) -> usize {
//     core_affinity::set_for_current(CoreId { id: 1 });
//     let mut msg = [0usize; N];
//     let mut t = 0;
//     while !done.load(Ordering::Relaxed) {
//         lock.read(&mut msg);
//         let first = msg[0];
//         for i in msg {
//             if first != i {
//                 t += 1;
//             }; // data consistency is verified here
//         }
//     }
//     return t;
// }
// #[inline(never)]
// fn writer<const N: usize>(done: &AtomicBool, lock: &SeqLock<[usize;N]>) {
//     core_affinity::set_for_current(CoreId { id: 10 });
//     let curt = Instant::now();
//     let mut count = 0usize;
//     let mut msg  = [0usize; N];
//     let mut msg1 = [0usize; N];

//     while curt.elapsed() < Nanos::from_secs(1) {
//         lock.write(&msg);
//         lock.write(&msg1);
//         count = count.wrapping_add(1);
//         msg.fill(count);
//         count = count.wrapping_add(1);
//         msg1.fill(count);
//         std::thread::sleep(std::time::Duration::from_micros(10) );
//     }
//     done.store(true, Ordering::Relaxed);
// }

// pub fn main() {
//     let lock = SeqLock::new([0usize; N]);
//     let done = AtomicBool::new(false);
//     std::thread::scope(|s| {
//         s.spawn(|| {
//             println!("{}", reader(&done, &lock));
//         });
//         s.spawn(|| {
//             writer(&done, &lock)
//         });

//     });
// }

#[derive(Clone, Copy)]
struct TimingMessage {
    rdtscp: Instant,
    data:   [u8; 1],
}

fn contender(lock: &Seqlock<TimingMessage>)
{
    let mut m = TimingMessage { rdtscp: Instant::now(), data: [0]};
    while m.data[0] == 0 {
        lock.read(&mut m);
    }
}

fn timed_consumer(lock: &Seqlock<TimingMessage>)
{
    let mut timer = Timer::new("read");
    core_affinity::set_for_current(CoreId { id: 1 });
    let mut m = TimingMessage { rdtscp: Instant::now(), data: [0]};
    let mut last = m.rdtscp;
    while m.data[0] == 0 {
        timer.start();
        lock.pessimistic_read(&mut m);
        if m.rdtscp != last {
            timer.stop_and_latency(m.rdtscp);
        }
        last = m.rdtscp;
    }
}

fn producer(lock: &Seqlock<TimingMessage>)
{
    let mut timer = Timer::new("write");
    core_affinity::set_for_current(CoreId { id: 2 });
    let mut m = TimingMessage { rdtscp: Instant::now(), data: [0]};
    let curt = Instant::now();
    while curt.elapsed() < Nanos::from_secs(6) {
        timer.start();
        m.rdtscp = Instant::now();
        lock.write(&m);
        timer.stop();
        let curt = Instant::now();
        while Instant::now() - curt < Nanos::from_micros(2) {}
    }
    m.data[0] = 1;
    lock.write(&m);
}

fn consumer_latency(n_contenders: usize) {
    let lock = Seqlock::default();
    std::thread::scope(|s| {
        for i in 1..(n_contenders + 1) {
            let lck = &lock;
            s.spawn(move || {
                core_affinity::set_for_current(CoreId { id: i + 2 });
                contender(lck);
            });
        }
        s.spawn(|| timed_consumer(&lock));
        s.spawn(|| producer(&lock));
    })
}

#[repr(align(64))]
struct Test
{
    flag: AtomicI32
}

const PING: bool = false;
const PONG: bool = true;

fn one_way_2_lines(n_samples:usize) {
    let seq1 = Test {flag: AtomicI32::new(-1)};
    let seq2 = Test {flag: AtomicI32::new(-1)};
    std::thread::scope(|s| {
        s.spawn(|| {
            core_affinity::set_for_current(CoreId { id: 1});
            for _ in 0..n_samples {
                for n in 0..10000 {
                    while seq1.flag.load(Ordering::Acquire) != n {}
                    seq2.flag.store(n, Ordering::Release);
                }
            }
        });
        s.spawn(|| {
            let mut timer = Timer::new("one_way_2_lines");
            core_affinity::set_for_current(CoreId { id: 3 });
            for _ in 0..n_samples {
                let mut c = 0;
                for n in 0..10000 {
                    timer.start();
                    seq1.flag.store(n, Ordering::Release);
                    while seq2.flag.load(Ordering::Acquire) != n {c+=1}
                    timer.stop();
                }
            }
        });
    });
}

fn one_way_2_lines_shared(n_samples:usize) {
    let seq = [Test {flag: AtomicI32::new(-1)}, Test {flag: AtomicI32::new(-1)}];
    std::thread::scope(|s| {
        s.spawn(|| {
            core_affinity::set_for_current(CoreId { id: 1});
            for _ in 0..n_samples {
                for n in 0..10000 {
                    while seq[0].flag.load(Ordering::Acquire) != n {}
                    seq[1].flag.store(n, Ordering::Release);
                }
            }
        });
        s.spawn(|| {
            let mut timer = Timer::new("one_way_2_lines");
            core_affinity::set_for_current(CoreId { id: 3 });
            for _ in 0..n_samples {
                let mut c = 0;
                for n in 0..10000 {
                    timer.start();
                    seq[0].flag.store(n, Ordering::Release);
                    while seq[1].flag.load(Ordering::Acquire) != n {c+=1}
                    timer.stop();
                }
            }
        });
    });
}

pub fn main() {
    // one_way_2_lines_shared(1000000);
    consumer_latency(0);
}
