+++
title = "Inter Core Communication Pt 1: SeqLock"
date = 2024-05-25
description = "A thorough investigation of the main synchronization primitive used by Mantra: the SeqLock"
[taxonomies]
tags =  ["mantra", "icc", "seqlock"]
[extra]
comment = true
+++

As the first technical topic in this blog, I will discuss the main method of inter core synchronization used in [**Mantra**](@/posts/hello_world/index.md): a `SeqLock`. It forms the fundamental building block for the "real" datastructures: `Queues` and `SeqLockVectors`, which will be the topic of the next blog post.

While designing the inter core communication (icc) layer,  have taken a great deal of inspiration from
- [Trading at light speed](https://www.youtube.com/watch?v=8uAW5FQtcvE) by David Gross
- An amazing set of references: [Awesome Lockfree](https://github.com/rigtorp/awesome-lockfree) by Erik Rigtorp

# Design Goals and Considerations

- Achieve a close to the ideal ~30-40ns core-to-core latency (see e.g. [anandtech 13900k and 13600k review](https://www.anandtech.com/show/17601/intel-core-i9-13900k-and-i5-13600k-review/5) and the [fantastic core-to-core-latency tool](https://github.com/nviennot/core-to-core-latency))
- data `Producers` do not care about and are not impacted by data `Consumers`
- `Consumers` should not impact eachother, or the system as a whole

# SeqLock
The embodiment of the above goals in terms of synchronization techniques is the `SeqLock` (see [Wikipedia](https://en.wikipedia.org/wiki/Seqlock), [seqlock in the linux kernel](https://docs.kernel.org/locking/seqlock.html), and [Erik Rigtorp's C++11 implementation](https://github.com/rigtorp/Seqlock)).

The essence can be boiled down to
- A `Producer` (or writer) is never blocked by `Consumers` (readers)
- The `Producer` atomically increments a counter (the `Seq` in `SeqLock`) once before and once after writing the data
- `counter & 1 == 0` (even) communicates to `Consumers` that they can read data
- `counter_before_read == counter_after_read`: data was consistent while reading
- Compare and swap can be used on the counter to allow multiple `Producers` to write to same `SeqLock`
- Compilers and cpus in general can't be trusted, making it crucial to verify that the execution sequence indeed follows the steps we instructed. Memory barriers and fences are required to guarantee this in general

# TL;DR
Out of solidarity with your scroll wheel, the final implementation is
```rust
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
```
That's it, till next time folks!

# Are barriers necessary?
Most literature on `SeqLocks` focuses (rightly so) on correctness and guaranteeing it as much as possible. The two main points are that
- the `data` has no dependency on the `version` of the `SeqLock`, i.e. the compiler is allowed to merge or reorder the two increments on the `Producer` side and the checks on the `Consumer` side
- depending on the model, the cpu can similarly reorder when changes to `version` become visible to the other cores, and when `data` is actually copied in and out of the lock

On x86 the latter is less of a problem since they are "strongly memory ordered" and `version` is atomic. However, adding the barriers in this case leads to no-ops so they don't hurt either. See the [Release-Acquire ordering section in the c++ reference](https://en.cppreference.com/w/cpp/atomic/memory_order#Release-Acquire_ordering) for further information.

## Torn data testing

Before demonstrating how one would go about verifiying if and why barriers are needed, let's design some tests to validate a `SeqLock` implementation.
The main concern is data consistency, i.e. that a `Consumer` does not read data that is being written to.
We test this by making a `Producer` fill and write an array with an increasing counter into the `SeqLock`, while a `Consumer` reads and verifies that all entries in the read array are identical (see the highlighted line below)

```rust,linenos,hl_lines=17
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
                        assert_eq!(first, i);
                    }
                }
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
        read_test::<65536>()
    }
}
```

If I run these tests on an intel i9 14900k, using the following more naive implementation
```rust
pub fn read(&self, result: &mut T) {
    loop {
        let v1 = self.version.load(Ordering::Relaxed);
        *result = unsafe { *self.data.get() };
        let v2 = self.version.load(Ordering::Relaxed);
        if v1 == v2 && v1 & 1 == 0 {
            return;
        }
    }
}

pub fn write(&self, val: &T) {
    let v = self.version.load(Ordering::Relaxed).wrapping_add(1);
    self.version.store(v, Ordering::Relaxed);
    unsafe { *self.data.get() = *val };
    self.version.store(v.wrapping_add(1), Ordering::Relaxed);
}
```
I find that they fail for array sizes of 64 (512 bytes) and up. This signals that either the compiler or the cpu did some reordering of operations.

The issue in this specific case is that the compiler has inline the `read` function and reordered `let first = msg[0]` on line (15) to be executed before
the `read` on line (14), causing obvious problems.
Just to highlight how fickle inlining is, adding `assert_ne!(msg[0], 0)` after line (19) makes all tests pass.

While changing one of the `self.version.load(Ordering::Relaxed)` operations in `read` to `self.version.load(Ordering::Acquire)` solves the issue in this case, adding `#[inline(never)]` is much more safe and does not lead to any difference in performance.
We do the same for the `write` function to avoid similar shenanigans there.

Even though the tests now pass even without using any barriers, we proceed with a double check by analyzing the assembly that was produced by the compiler.

## Deeper dive using [`cargo asm`](https://crates.io/crates/cargo-show-asm/0.2.34)
This is for sure one of the best tools for this kind of analysis. I highly recommend adding `#[inline(never)]` to isolate the function of interest from the rest of the code.

### `SeqLock::<[usize; 1024]>::read`

```asm, linenos, hl_lines=19 22 26 27 28 30
code::SeqLock<T>::read:
        .cfi_startproc
        push r15
        .cfi_def_cfa_offset 16
        push r14
        .cfi_def_cfa_offset 24
        push r12
        .cfi_def_cfa_offset 32
        push rbx
        .cfi_def_cfa_offset 40
        push rax
        .cfi_def_cfa_offset 48
        .cfi_offset rbx, -40
        .cfi_offset r12, -32
        .cfi_offset r14, -24
        .cfi_offset r15, -16
        mov rbx, rsi
        mov r14, rdi
        mov r15, qword ptr [rip + memcpy@GOTPCREL]
        .p2align        4, 0x90
.LBB6_1:
        mov r12, qword ptr [r14 + 8192]
        mov edx, 8192
        mov rdi, rbx
        mov rsi, r14
        call r15
        mov rax, qword ptr [r14 + 8192]
        test r12b, 1
        jne .LBB6_1
        cmp r12, rax
        jne .LBB6_1
        add rsp, 8
        .cfi_def_cfa_offset 40
        pop rbx
        .cfi_def_cfa_offset 32
        pop r12
        .cfi_def_cfa_offset 24
        pop r14
        .cfi_def_cfa_offset 16
        pop r15
        .cfi_def_cfa_offset 8
        ret
```
First thing we can observe in lines (19, 22, 27) is that `rust` chose to not adhere to our ordering of fields in `SeqLock`, i.e. it moved `version` behind `data`.
If needed, the order of fields can be preserved by adding `#[repr(C)]`.

The lines that constitute the main operations of the `SeqLock` are highlighted, corresponding almost one-to-one with the `read` function:
1. assign function pointer to `memcpy` to `r15` for faster future calling
2. move `version` at `SeqLock start (r14) + 8192 bytes` into `r12`
3. perform the `memcpy`
4. move `version` at `SeqLock start (r14) + 8192 bytes` into `rax`
5. check `r12 & 1 == 0`
6. check `r12 == rax`
7. Profit...

### `SeqLock::<[usize; 1]>::read`
```asm, linenos
code::SeqLock<T>::read:
        .cfi_startproc
        mov rax, qword ptr [rdi + 8]
        .p2align        4, 0x90
.LBB6_1:
        mov rcx, qword ptr [rdi]
        mov rdx, qword ptr [rdi]
        test cl, 1
        jne .LBB6_1
        cmp rcx, rdx
        jne .LBB6_1
        mov qword ptr [rsi], rax
        ret
```
Well at least it looks clean... I'm pretty sure I don't have to underline the issue with steps
1. Do the copy into `rax`
2. move the **version** into `rcx` and `rdx`
3. test like before, I mean why even do this
4. copy from `rax` into the input
5. No Stonks...

This showcases again that while tests are useful it is good practice to double check that the compiler produced the correct assembly.
In fact, I never got the tests to fail after adding the `#[inline(never)]` we discussed earlier, even though the assembly clearly shows that nothing stops a read while the write is happening.
The reason behind this is that the `memcpy` is done **inline/in cache** for small enough data using moves between cache and registers (`rax` in this case).
If a single instruction is used (`mov` here) it is never possible that the data is partially overwritten while reading, and it is still highly unlikely when multiple instructions are required.

### Adding Memory Barriers
Here we go:
```rust, linenos
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
```
```asm, linenos
code::SeqLock<T>::read:
        .cfi_startproc
        .p2align        4, 0x90
.LBB6_1:
        mov rax, qword ptr [rdi]
        #MEMBARRIER
        mov rcx, qword ptr [rdi + 8]
        mov qword ptr [rsi], rcx
        #MEMBARRIER
        mov rcx, qword ptr [rdi]
        test al, 1
        jne .LBB6_1
        cmp rax, rcx
        jne .LBB6_1
        ret
```
It is interesting to see that the compiler chooses to reuse `rcx` both for the data copy in lines (5) and (6), as well as the second version load in line (8).

With the current `rust` compiler (1.78.0), I found that only adding `Ordering::Acquire` in lines (4) or (7) already does the trick.
However, they only guarantee the ordering of loads of the `version` atomic when combined with an `Ordering::Release` store in the `write` function, not when the actual data is copied.
That is where the `compiler_fence` comes in guaranteeing also this ordering. I have not noticed a change in performance when adding these additional barriers.

The corresponding `write` function becomes:
```rust
#[inline(never)]
pub fn write(&self, val: &T) {
    let v = self.version.load(Ordering::Relaxed).wrapping_add(1);
    self.version.store(v, Ordering::Release);
    compiler_fence(Ordering::AcqRel);
    unsafe { *self.data.get() = *val };
    compiler_fence(Ordering::AcqRel);
    self.version.store(v.wrapping_add(1), Ordering::Release);
}
```
Our `SeqLock` implementation should now be correct and is in fact pretty much identical to others around. Next up is something that's covered much less frequently: timing and potentially optimizing the implementation.
There is not much room to play with here, but it serves as a good first demonstration of some basic concepts that surround timing low latency constructs, and give a glimpse into the inner workings of the cpu.
BTW, if memory models and barriers are really your schtick, live a little and marvel your way through [The Linux Kernel Docs on Memory Barriers](https://docs.kernel.org/core-api/wrappers/memory-barriers.html).

# Performance
Does the `fetch_add` make the `write` function of the [final implementation](@/posts/icc_1_seqlock/index.md#tl-dr) indeed faster?

## Timing 101
The full details regarding the suite of timing and performance measurements tools I have developed to track the performance of [**Mantra**](@/posts/hello_world/index.md) will be divulged in a later post.

For now, the key points are:

**Use `rdtscp` to take timestamps**: the `rdtscp` hardware counter is a monotonously increasing cpu cycle counter (at base frequency) which is reset upon startup. What's even better is that on recent cpus it is shared between all cores (look for `constant_tsc` in `/proc/cpuinfo`). It is the cheapest, at ~5ns overhead, and most precise way to take timestamps. An added benefit is that it partially orders operations (see discussion above), in that it will not execute until _"all previous instructions have executed and all previous loads are globally visible"_ see [this](https://www.felixcloutier.com/x86/rdtscp). Using an `_mm_lfence` after the initial `rdtscp` will also force executions to not begin before the timestamp is taken. This is **the only reasonable way** to time on really low latency scales.

**use [`core_affinity`](https://docs.rs/core_affinity/latest/core_affinity/) and `isolcpus`**: The combination of the [`isolcpus`](https://wiki.linuxfoundation.org/realtime/documentation/howto/tools/cpu-partitioning/isolcpus) kernel parameter with binding a thread in `rust` to a specific core allows us to minimize jitter coming from whatever else is running on the computer. I have isolated the p-cores on my cpu for our testing purposes. See [Erik Rigtorp's low latency tuning guide](https://rigtorp.se/low-latency-guide/) for even more info.

**Offload the actual timing**: To minimize the timing overhead we take the two `rdtscp` stamps and offload them to a `Queue` in shared memory (more on what a `Queue` is later). Another process can then read these messages, collect statistics and convert `rdtscp` stamp deltas to nanoseconds (in the i9 14900k case x3.2). For this last step we can actually reuse the [`nanos_from_raw_delta`](https://docs.rs/quanta/latest/quanta/struct.Clock.html#method.delta_as_nanos) function in the `quanta` library.


Putting it all together, a block of code can be timed like:
```rust
let t1 = unsafe { __rdtscp(&mut 0u32 as *mut _) };
unsafe { _mm_lfence() };
// code to be timed
let t2 = unsafe { __rdtscp(&mut 0u32 as *mut _) };
timer_queue.produce((t1, t2));
```
or, using my timing library
```rust
let mut timer = Timer::new("my_cool_timer");
timer.start();
//code to be timed
timer.stop();
```
with an added functionality where you can use a previously taken `rdtscp` timestamp to measure a `latency`:
```rust
timer.latency(prev_rdtscp);
```
The former will be called `Business` timing (for business logic), and the latter, you guessed it, `Latency` timing.

I've created a tui tool called `timekeeper` that ingests these timing results and can be ran separate from the timed code. It then displays a continuously updating graph of timings and some statistics:

![](timekeeper_example.png#noborder "timekeeper_example")
*Fig 1. Timekeeper example*

## Baseline Inter Core Latency

After isolating cpus, turning off hyperthreading, doing some more of the low latency tuning steps and switching to the `performance` governor, I use the following basic ping/pong code to provide us with a baseline:
```rust
#[repr(align(64))]
struct Test(AtomicI32);

fn one_way_2_lines(n_samples:usize) {
    let seq1 = Test(AtomicI32::new(-1));
    let seq2 = Test(AtomicI32::new(-1));
    std::thread::scope(|s| {
        s.spawn(|| {
            core_affinity::set_for_current(CoreId { id: 2});
            for _ in 0..n_samples {
                for n in 0..10000 {
                    while seq1.0.load(Ordering::Acquire) != n {}
                    seq2.0.store(n, Ordering::Release);
                }
            }
        });
        s.spawn(|| {
            let mut timer = Timer::new("one_way_2_lines");
            core_affinity::set_for_current(CoreId { id: 3 });
            for _ in 0..n_samples {
                for n in 0..10000 {
                    timer.start();
                    seq1.0.store(n, Ordering::Release);
                    while seq2.0.load(Ordering::Acquire) != n {}
                    timer.stop();
                }
            }
        });
    });
}
```

The `2_lines` stands for the fact that we are communicating through atomics `seq1` and `seq2` with each their own cacheline: i.e. `#[repr(align(64))]`.
Running the code multiple times leads to:

![](one_way_2_lines.png#noborder "baseline_inter_core")
*Fig 2. Base line core-core latency*

Bear in mind that these are round trip times making the real latency half of what is measured.

The steps with different but constant average timings showcases the main difficulty with timing low level/low latency constructs:
the cpu is essentially a black box and does a lot of memory and cache related wizardry behind the scenes to implement the [MESI protocol](https://en.wikipedia.org/wiki/MESI_protocol).
Combined with branch prediction this makes the final result quite dependent on the exact execution starting times of the threads, leading to different but stable averages each run.
 
Anyway, the lower end of these measurements serves as a sanity check and target for our `Seqlock` latency.

## SeqLock performance
In all usecases of `Seqlocks` in [**Mantra**](@/posts/hello_world/index.md) there are one or many `Producers` which 99% of the time don't produce anything, while `Consumers` are busy spinning on the last `SeqLock` until it gets written to.

We reflect this in the timing code's setup:
- a `Producer` writes an `rdtscp` timestamp into the `Seqlock` every 2 microseconds
- a `Consumer` busy spins reading this timestamp, and if it changes publishes a timing and latency measurement using it as the starting point
- 0 or more "contender" `Consumers` do the same to see how increasing consumer count impacts the main `Producer` and `Consumer`

```rust
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
```

### Starting Point
We use the `SeqLock` code we implemented above, leading to the following latency timings for a single consumer (left) and 5 consumers (right):

![](consumer_latency_initial.png#noborder "initial_consumer_latency")
*Fig 3. Initial Consumer Latency*

Would you look at that: timings are very stable without much jitter (tuning works!), the latency increase with increasing `Consumer` count is extremely minimal while the `Producer` gets... even faster(?), somehow.
I triple checked and it is consistently reproducible.

However, we are quite far off the ~30-40ns latency target. Looking closer at the write function, we can realize that `fetch_add` is a single instruction version of lines (1) and (2):
```rust, linenos, hl_lines= 1 2
    let v = self.version.load(Ordering::Relaxed).wrapping_add(1);
    self.version.store(v, Ordering::Release);
    compiler_fence(Ordering::AcqRel);
    unsafe { *self.data.get() = *val };
    compiler_fence(Ordering::AcqRel);
    self.version.store(v.wrapping_add(1), Ordering::Release);
```
which leads to:
```rust
    let v = self.version.fetch_add(1, Ordering::Release);
    compiler_fence(Ordering::AcqRel);
    unsafe { *self.data.get() = *val };
    compiler_fence(Ordering::AcqRel);
    self.version.store(v.wrapping_add(2), Ordering::Release);
```
leads to a serious improvement especially for the 1 `Consumer` case:

![](consumer_latency_improved.png#noborder "improved_consumer_latency")
*Fig 4. Optimized Consumer Latency*

As far as I can tell, there is nothing that can be optimized on the `read` side of things.

One final optimization is to add `#[repr(align(64))]` the `SeqLocks`: 
```rust
#[repr(align(64))]
pub struct SeqLock<T> {
    version: AtomicUsize,
    data: UnsafeCell<T>,
}
```
This fixes potential [`false sharing`](https://en.wikipedia.org/wiki/False_sharing) issues by never having two or more `SeqLocks` on a single cache line.
While it is not very important when using a single `SeqLock`, it becomes crucial when using them inside `Queues` and `SeqLockVectors`.

Looking back at our original design goals:
- close to minimum inter core latency
- `Producers` are never blocked
- `Consumers` don't impact the `Producers` and themselves + adding more `Consumers` doesn't dramatically decrease performance

Our implementation seems to be as good as it can be!

This concludes our deep dive into `SeqLocks` as this first technical blog post.
We've laid the groundwork and have introduced some important concepts for the upcoming post on `Queues` and `SeqLockVectors` as Pt 2 on inter core communication.

See you then!

# Possible future investigations/improvements
- Use the [`cldemote`](https://www.felixcloutier.com/x86/cldemote) to force the `Producer` to immediately flush the `SeqLock` data to the consumers
- [UMONITOR/UMWAIT spin-wait loop](https://stackoverflow.com/questions/74956482/working-example-of-umonitor-umwait-based-assembly-asm-spin-wait-loops-as-a-rep#)

