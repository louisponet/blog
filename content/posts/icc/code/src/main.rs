use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::arch::x86_64::{__rdtscp, _mm_lfence};
use std::time::Duration;

use code::SeqLock;
// use ma_queues::seqlock::SeqLock;
use core_affinity::CoreId;
use ma_time::{Instant, Nanos};
use ma_timing::Timer;
use rand::Rng;

#[derive(Clone, Copy)]
#[repr(C)]
struct TimingMessage<const N: usize> {
    rdtscp: Instant,
    data: [u8;N]
}
impl<const N: usize> Default for TimingMessage<N> {
    fn default() -> Self {
        Self {
            rdtscp: Instant::default(),
            data: [0; N],
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
fn consumer_latency<const N_BYTES: usize>(n_contenders: usize){
    std::thread::scope(|s| {
        let ctrlcd = Arc::new(AtomicBool::new(false));
        let ctrlcd1 = ctrlcd.clone();
        ctrlc::set_handler(move || {
            ctrlcd1.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        let lock = Arc::new(SeqLock::default());
        let lock1 = lock.clone();
        for i in 1..(n_contenders+1) {
            let lock2 = lock.clone();
            let ctrlcd2 = ctrlcd.clone();
            s.spawn(move || {
                core_affinity::set_for_current(CoreId { id: 2 * i + 3 });
                let mut m = TimingMessage::<N_BYTES>::default();
                while !ctrlcd2.load(Ordering::Relaxed) {
                    let m = lock2.read(&mut m);
                }
            });
        }
        let ctrlcd3 = ctrlcd.clone();
        s.spawn(move || {
            let mut timer = Timer::new("my_cool_timer");
            core_affinity::set_for_current(CoreId { id: 8 });
            let lck = lock.as_ref();
            let mut rng = rand::thread_rng();
            let mut m = TimingMessage::<N_BYTES>::default();
            let mut last = Instant::now();
            while !ctrlcd3.load(Ordering::Relaxed) {
                let t = Instant::now();
                unsafe{_mm_lfence()};
                lck.read(&mut m);
                let t2 = Instant::now();
                if m.rdtscp != last {
                    timer.set_start(t);
                    timer.set_stop(t2);
                    timer.send_cur();
                    timer.latency_till_stop(m.rdtscp);
                    let t = rng.gen_range(1000..3300);
                    let curt = rdtscp();
                    while rdtscp() - curt < t {}
                }
                last = m.rdtscp;
            }
        });
        s.spawn(move || {
            let mut timer = Timer::new("write");
            core_affinity::set_for_current(CoreId { id: 6 });
            let mut m = TimingMessage::<N_BYTES>::default();
            let lck = lock1.as_ref();
            while !ctrlcd.load(Ordering::Relaxed) {
                timer.start();
                m.rdtscp = Instant::now();
                lck.write(&m);
                timer.stop();
                // let last_write = rdtscp();
                // let t = rng.gen_range(1000..3300);
                let curt = rdtscp();
                while rdtscp() - curt < 33000 {}
            }
        });
    })
}

const PING: bool = false;
const PONG: bool = true;
fn cas_baseline(){
        let done = AtomicBool::new(false);
        let flag = AtomicBool::new(PING);
        let barrier = Barrier::new(2);
        println!("starting");
        std::thread::scope(|s| {
            s.spawn(|| {
                core_affinity::set_for_current(CoreId { id: 8});

                barrier.wait();
                while !done.load(Ordering::Relaxed){
                    while flag.compare_exchange(PING, PONG, Ordering::Relaxed, Ordering::Relaxed).is_err(){
                        if done.load(Ordering::Relaxed){break}
                    }
                }
                println!("pingpong done");
            });
            s.spawn(|| {
                let mut timer = Timer::new("cas_round_trip");
                core_affinity::set_for_current(CoreId { id: 6});
                barrier.wait();
                while !done.load(Ordering::Relaxed) {
                    timer.start();
                    let curt = std::time::Instant::now();
                    for _ in 0..1000 {
                        while flag.compare_exchange(PONG, PING, Ordering::Relaxed, Ordering::Relaxed).is_err() {
                            if done.load(Ordering::Relaxed){
                                break;
                            }
                        }
                    }
                    println!("{:?}", curt.elapsed());
                    timer.stop();
                }
                println!("pongping done");
            });
            std::thread::sleep(Duration::from_secs(2));
            done.store(true, Ordering::Relaxed);
        });
}

pub fn main() {
    // consumer_latency::<1>(0);
    cas_baseline();
        std::thread::sleep(Duration::from_secs(2));
    cas_baseline();
        std::thread::sleep(Duration::from_secs(5));
    cas_baseline();
        std::thread::sleep(Duration::from_secs(6));
    cas_baseline();
        std::thread::sleep(Duration::from_secs(6));
    cas_baseline();
}

