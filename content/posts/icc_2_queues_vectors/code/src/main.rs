use std::arch::x86_64::{__rdtscp, _mm_clflush, _mm_lfence};
use std::sync::atomic::{compiler_fence, fence, AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::time::Duration;

use code::SeqLock;
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

fn contender(lock: &SeqLock<TimingMessage>)
{
    let mut m = TimingMessage { rdtscp: Instant::now(), data: [0]};
    while m.data[0] == 0 {
        lock.read(&mut m);
    }
}

fn timed_consumer(lock: &SeqLock<TimingMessage>)
{
    let mut timer = Timer::new("read");
    core_affinity::set_for_current(CoreId { id: 1 });
    let mut m = TimingMessage { rdtscp: Instant::now(), data: [0]};
    let mut last = m.rdtscp;
    while m.data[0] == 0 {
        timer.start();
        lock.read(&mut m);
        if m.rdtscp != last {
            timer.stop();
            timer.latency_till_stop(m.rdtscp);
        }
        last = m.rdtscp;
    }
}

fn producer(lock: &SeqLock<TimingMessage>)
{
    let mut timer = Timer::new("write");
    core_affinity::set_for_current(CoreId { id: 2 });
    let mut m = TimingMessage { rdtscp: Instant::now(), data: [0]};
    let curt = Instant::now();
    while curt.elapsed() < Nanos::from_secs(5) {
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
    let lock = SeqLock::default();
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
            core_affinity::set_for_current(CoreId { id: 2});
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

pub fn main() {
    // one_way_2_lines(1000000);
    consumer_latency(0);
}
