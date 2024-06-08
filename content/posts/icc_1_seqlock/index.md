+++
title = "Inter Core Communication Pt 1: Seqlock"
date = 2024-05-25
description = "A thorough investigation of the main synchronization primitive used by Mantra: the Seqlock"
[taxonomies]
tags =  ["mantra", "icc", "seqlock"]
[extra]
comment = true
+++

As the first technical topic in this blog, I will discuss the main method of synchronizing the inter core communication used in [**Mantra**](@/posts/hello_world/index.md): a `Seqlock`.
It forms the fundamental building block for the "real" communication datastructures: `Queues` and `SeqlockVectors`, which will be the topic of the next blog post.

After first considering the requirements that made me choose the `Seqlock` for **Mantra**, I will not make those in a hurry wait and get straight to the final implementation.

For those still interested after that, I will continue by discussing how to verify the correctness of a `Seqlock` implementation, and potentially use memory barriers to make it reliable.
This will involve designing tests, observing the potential pitfalls of function inlining, looking at some assembly code (funky), and strong-arming the compiler to do our bidding.

Finally, we go through a quick 101 on low-latency timing that we use to gauge and optimize the performance of the implementation.

Before continuing, I would like to give major credit to everyone involved with creating the following inspirational material
- [Trading at light speed](https://www.youtube.com/watch?v=8uAW5FQtcvE)
- An amazing set of references: [Awesome Lockfree](https://github.com/rigtorp/awesome-lockfree)
- [C++ atomics, from basic to advanced. What do they really do?](https://www.youtube.com/watch?v=ZQFzMfHIxng)

# Design Goals and Considerations

- Achieve a close to the ideal ~30-40ns core-to-core latency (see e.g. [anandtech 13900k and 13600k review](https://www.anandtech.com/show/17601/intel-core-i9-13900k-and-i5-13600k-review/5) and the [fantastic core-to-core-latency tool](https://github.com/nviennot/core-to-core-latency))
- data `Producers` do not care about and are not impacted by data `Consumers`
- `Consumers` should not impact eachother

# Seqlock
The embodiment of the above goals in terms of synchronization techniques is the `Seqlock` (see [Wikipedia](https://en.wikipedia.org/wiki/Seqlock), [seqlock in the linux kernel](https://docs.kernel.org/locking/seqlock.html), and [Erik Rigtorp's C++11 implementation](https://github.com/rigtorp/Seqlock)).

The key points are:
- A `Producer` (or writer) is never blocked by `Consumers` (readers)
- The `Producer` atomically increments a counter (the `Seq` in `Seqlock`) once before and once after writing the data
- `counter & 1 == 0` (even) communicates to `Consumers` that they can read data
- `counter_before_read == counter_after_read`: data remained consistent while reading
- Compare and swap could be used on the counter to allow multiple `Producers` to write to same `Seqlock`
- Compilers and cpus in general can't be trusted, making it crucial to verify that the execution sequence indeed follows the steps we instructed. Memory barriers and fences are required to guarantee this in general

# TL;DR
Out of solidarity with your scroll wheel and without further ado:
```rust
#[derive(Default)]
#[repr(align(64))]
pub struct Seqlock<T> {
    version: AtomicUsize,
    data: UnsafeCell<T>,
}
unsafe impl<T: Send> Send for Seqlock<T> {}
unsafe impl<T: Sync> Sync for Seqlock<T> {}

impl<T: Copy> Seqlock<T> {
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
Most literature on `Seqlocks` focuses (rightly so) on guaranteeing correctness.

Because the stored `data` does not depend on the `version` of the `Seqlock`, the compiler is allowed to merge or reorder the two increments to the `version` in the `write` function.
The same goes for the checks on `v1` and `v2` on the `read` side of things.
It potentially gets worse, though:
depending on the architecture of the cpu, read and write operations to `version` and `data` could be reordered **on the hardware level**.
Given that the `Seqlock's` correctness depends entirely on the sequence of `version` increments and checks around `data` writes and reads, these issues are big no-nos.

As we will investigate further below, memory barriers are the main solution to these issues.
They keep the compiler in line by guaranteeing [certain things](https://en.cppreference.com/w/cpp/atomic/memory_order), forcing it to adhere to the sequence of instructions that we specified in the code.
The same applies to the cpu itself.

For x86 cpus, the memory barriers luckily do not require any *additional* cpu instructions, just that no instructions are reordered or ommitted.
These cpus are [strongly memory ordered](https://www.cl.cam.ac.uk/~pes20/weakmemory/cacm.pdf), meaning that they guarantee the following: writes to some memory (i.e. `version`) can not be reordered with writes to other memory (i.e. `data`), and similar for reads.
Other cpu architectures might require additional cpu instructions to enforce these guarantees.
As long as we include the barriers, the `rust` compiler can figure out for us whether these are needed.

See the [Release-Acquire ordering section in the c++ reference](https://en.cppreference.com/w/cpp/atomic/memory_order#Release-Acquire_ordering) for further information on the specific barrier construction that is used in the `Seqlock`.

## Torn data testing

The first concern that we can relatively easily verify is data consistency.
In the test below we verify that when a `Consumer` supposedly succesfully reads `data`, the `Producer` was indeed not simultaneously writing to it.
We do this by making a `Producer` fill and write an array with an increasing counter, while a `Consumer` reads and verifies that all entries in the array are identical (see the highlighted line below).
If reading and writing were to happen at the same time, the `Consumer` would at some point see partially new and partially old data with differing counter values.
```rust,linenos,hl_lines=14 15 17 26 27
#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::atomic::AtomicBool, time::{Duration, Instant}};

    fn read_test<const N: usize>()
    {
        let lock = Seqlock::new([0usize; N]);
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

If I run these tests on an intel i9 14900k, using the following simplified `read` and `write` implementations without memory barriers
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

## Inline, and Compiler Cleverness
Funnily enough, barriers are not necessarily needed to fix these tests. Yeah, I also was not happy that my illustrating example in fact does not illustrate what I was trying to illustrate.

Nonetheless, I chose to mention it because it highlights just how much the compiler will mangle your code if you let it.
I will not paste the resulting assembly here as it is rather lengthy (see [assembly lines (23, 50-303) on godbolt](https://godbolt.org/z/7MYaW7Pba)).
The crux is that the compiler chose to inline the `read` function and then decided to move the `let first = msg[0]` statement of line (15) entirely outside of the `while` loop...

Strange? Maybe not.
The compiler's reasoning here is actually similar to the one that requires us to use memory barriers.
The essential point is, again, that the `data` field inside the `Seqlock` is not an atomic variable like `version`.
This allows the compiler to assume that only the current thread touches it. Meanwhile, the `Consumer` thread never writes to `data`, so it never changes, right?
Ha, might as well just set `first = data[0]` once and for all before starting with the actual `read` & verify loop.
Of course, the reality is that the `Producer` is actually changing `data`. Thus, as soon as the `Consumer` thread `reads` it into `msg`, `first != msg[i]` causing our test to fail.

Interestingly, adding `assert_ne!(msg[0], 0)` after line (19) seems to make the compiler less sure about this code transformation because suddenly all tests pass.
Looking at the resulting assembly confirms this observation as now line (15) is correctly executed each loop after first reading the `Seqlock`.

The first step towards provable correctness of the `Seqlock` is thus to add `#[inline(never)]` to the `read` and `write` functions.

## Deeper dive using [`cargo asm`](https://crates.io/crates/cargo-show-asm/0.2.34)
I kind of jumped the gun above with respect to reading compiler produced assembly. The tool I use by far the most for this is `cargo asm`.
It can be easily installed using `cargo` and has a very user friendly terminal based interface.
[godbolt](https://godbolt.org) is another great choice, but it can become tedious to copy-paste all the supporting code when working on a larger codebase.
In either case, I recommend adding `#[inline(never)]` to the function of interest so its assembly can be more easily filtered out.

Let's see what the compiler generates for the `read` function of a couple different array sizes.
### `Seqlock::<[usize; 1024]>::read`
When using a large array with 1024 elements, the assembly reads
```asm, linenos, hl_lines=19 22 26 27 28 30
code::Seqlock<T>::read:
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
The first thing we observe in lines (19, 22, 27) is that the compiler chose not to adhere to the ordering of fields in our definition of the `Seqlock`, moving `version` behind `data`.
If needed, the order of fields can be preserved by adding `#[repr(C)]`.

The operational part of the `read` function is, instead, almost one-to-one translated into assembly:
1. assign function pointer to `memcpy` to `r15` for faster future calling
2. move `version` at `Seqlock start (r14) + 8192 bytes` into `r12`
3. perform the `memcpy`
4. move `version` at `Seqlock start (r14) + 8192 bytes` into `rax`
5. check `r12 & 1 == 0`
6. check `r12 == rax`
7. Profit...

### `Seqlock::<[usize; 1]>::read`
For smaller array sizes we get
```asm, linenos
code::Seqlock<T>::read:
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
1. Do the copy of `data` into `rax`
2. move `version` into `rcx`... and `rdx`?
3. test `version & 1 != 1`
4. test `rcx == rdx`... hol' on, what?
4. copy from `rax` into the input
5. wait a minute...

This is a good demonstration of why tests should not be blindly trusted and why double checking the produced assembly is good practice.
In fact, I never got the tests to fail after adding the `#[inline(never)]` discussed earlier, even though the assembly clearly shows that nothing stops a `read` while a `write` is happening.
This happens because the `memcpy` is done **inline/in cache** for small enough data, using moves between cache and registers (`rax` in this case).
If a single instruction is used (`mov` here) it is never possible that the data is partially overwritten while reading, and it remains highly unlikely even when multiple instructions are required.

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
code::Seqlock<T>::read:
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
It is interesting to see that the compiler chooses to reuse `rcx` both for the data copy in lines (5) and (6), as well as the second `version` load in line (8).

With the current `rust` compiler (1.78.0), I found that only adding `Ordering::Acquire` in lines (4) or (7) of the `rust` code already does the trick.
However, they only guarantee the ordering of loads of the atomic `version` when combined with an `Ordering::Release` store in the `write` function, not when the actual data is copied in relation to it.
That is where the `compiler_fence` comes in, guaranteeing also this ordering. As discussed before, adding these extra barriers in the code did not change the performance on x86.

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
Our `Seqlock` implementation should now be correct and is in fact pretty much identical to others that can be found in the wild.

Having understood a thing or two about memory barriers while solidifying our `Seqlock`, we now turn to an aspect that's covered much less frequently: timing and potentially optimizing the implementation.
Granted, there is not much room to play with here given the size of the functions.
Nevertheless, some of the key concepts that I will discuss in the process will be used in many future posts.

P.S.: if memory models and barriers are really your schtick, live a little and marvel your way through [The Linux Kernel Docs on Memory Barriers](https://docs.kernel.org/core-api/wrappers/memory-barriers.html).

# Performance
THe main question we will answer is: Does the `fetch_add` make the `write` function of the [final implementation](@/posts/icc_1_seqlock/index.md#tl-dr) indeed faster?

## Timing 101
The full details regarding the suite of timing and performance measurements tools I have developed to track the performance of [**Mantra**](@/posts/hello_world/index.md) will be divulged in a later post.

For now, the key points are:

**Use `rdtscp` to take timestamps**: the `rdtscp` hardware counter is a monotonously increasing cpu cycle counter (at base frequency) which is reset upon startup. What's even better is that on recent cpus it is shared between all cores (look for `constant_tsc` in `/proc/cpuinfo`). It is the cheapest, at ~5ns overhead, and most precise way to take timestamps. An added benefit is that it partially orders operations (see discussion above), in that it will not execute until _"all previous instructions have executed and all previous loads are globally visible"_, see [this](https://www.felixcloutier.com/x86/rdtscp). Using an `_mm_lfence` after the initial `rdtscp` will also force executions to not begin before the timestamp is taken. This is **the only reasonable way** to time on really low latency scales.

**use [`core_affinity`](https://docs.rs/core_affinity/latest/core_affinity/) and `isolcpus`**: The combination of the [`isolcpus`](https://wiki.linuxfoundation.org/realtime/documentation/howto/tools/cpu-partitioning/isolcpus) kernel parameter with binding a thread in `rust` to a specific core allows us to minimize jitter coming from whatever else is running on the computer. The p-cores on my cpu have been isolated for our testing purposes below. See [Erik Rigtorp's low latency tuning guide](https://rigtorp.se/low-latency-guide/) for even more info.

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

Throughout the following discussion we'll use a small tui tool I've created called `timekeeper` that ingests and displays these timing results:

![](timekeeper_example.png#noborder "timekeeper_example")
*Fig 1. Timekeeper example*

## Baseline Inter Core Latency

After isolating cpus, turning off hyperthreading, doing some more of the low latency tuning steps and switching to the `performance` governor, I've ran the following basic ping/pong code to provide us with some baseline latency timings:
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

The `"2_lines"` stands for the fact that we are communicating through atomics `seq1` and `seq2` with each their own cacheline: i.e. `#[repr(align(64))]`.
Running the code multiple times leads to:

![](one_way_2_lines.png#noborder "baseline_inter_core")
*Fig 2. Base line core-core latency*

Bear in mind that the real latency is half of what is measured since these are round trip times.

The steps with different but constant average timings showcases the main difficulty with timing low level/low latency constructs:
the cpu is essentially a black box and does a lot of memory and cache related wizardry behind the scenes to implement the [MESI protocol](https://en.wikipedia.org/wiki/MESI_protocol).
Combining this with branch prediction renders the final result quite dependent on the exact execution starting times of the threads, leading to different but stable averages each run.
 
Anyway, the lower end of these measurements serves as a sanity check and target for our `Seqlock` latency.

## Seqlock performance
In all usecases of `Seqlocks` in [**Mantra**](@/posts/hello_world/index.md) there are one or many `Producers` which 99% of the time don't produce anything, while `Consumers` are busy spinning on the last `Seqlock` until it gets written to.

We reflect this in the timing code's setup:
- a `Producer` writes an `rdtscp` timestamp into the `Seqlock` every 2 microseconds
- a `Consumer` busy spins reading this timestamp, and if it changes publishes a timing and latency measurement using it as the starting point
- 0 or more "contender" `Consumers` do the same to see how increasing `Consumer` contention impacts the main `Producer` and `Consumer`

```rust
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
        lock.read(&mut m);
        if m.rdtscp != last {
            timer.stop();
            timer.latency_till_stop(m.rdtscp);
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
```

### Starting Point
We use the `Seqlock` code we implemented above as the initial point, leading to the following latency timings for a single consumer (left) and 5 consumers (right):

![](consumer_latency_initial.png#noborder "initial_consumer_latency")
*Fig 3. Initial Consumer Latency*

Would you look at that: timings are very stable without much jitter (tuning works!), the latency increase with increasing `Consumer` count is extremely minimal while the `Producer` gets... even faster(?), somehow.
I triple checked and it is consistently reproducible.

### Optimization
We are, however, still quite far off the ~30-40ns latency target. Looking closer at the `write` function, we realize that `fetch_add` is a single instruction version of lines (1) and (2):
```rust, linenos, hl_lines= 1 2
    let v = self.version.load(Ordering::Relaxed).wrapping_add(1);
    self.version.store(v, Ordering::Release);
    compiler_fence(Ordering::AcqRel);
    unsafe { *self.data.get() = *val };
    compiler_fence(Ordering::AcqRel);
    self.version.store(v.wrapping_add(1), Ordering::Release);
```
which we thus change to:
```rust
    let v = self.version.fetch_add(1, Ordering::Release);
    compiler_fence(Ordering::AcqRel);
    unsafe { *self.data.get() = *val };
    compiler_fence(Ordering::AcqRel);
    self.version.store(v.wrapping_add(2), Ordering::Release);
```

Measuring again, we find that this leads to a serious improvement, almost halving the latency in the 1 `Consumer` case (left), while also slightly improving the 5 `Consumer` case (right):

![](consumer_latency_improved.png#noborder "improved_consumer_latency")
*Fig 4. Optimized Consumer Latency*

Unfortunately, there is nothing that can be optimized on the `read` side of things.

One final optimization we'll proactively do is to add `#[repr(align(64))]` the `Seqlocks`:
```rust
#[repr(align(64))]
pub struct Seqlock<T> {
    version: AtomicUsize,
    data: UnsafeCell<T>,
}
```
This fixes potential [`false sharing`](https://en.wikipedia.org/wiki/False_sharing) issues by never having two or more `Seqlocks` on a single cache line.
While it is not very important when using a single `Seqlock`, it becomes crucial when using them inside `Queues` and `SeqlockVectors`.

Looking back at our original design goals:
- close to minimum inter core latency
- `Producers` are never blocked
- `Consumers` don't impact the `Producers` and themselves + adding more `Consumers` doesn't dramatically decrease performance

our implementation seems to be as good as it can be!

We thus conclude our deep dive into `Seqlocks` here, also concluding this first technical blog post.
We've laid the groundwork and have introduced some important concepts for the upcoming post on `Queues` and `SeqlockVectors` as Pt 2 on inter core communication.

See you then!

# Possible future investigations/improvements
- Use the [`cldemote`](https://www.felixcloutier.com/x86/cldemote) to force the `Producer` to immediately flush the `Seqlock` data to the consumers
- [UMONITOR/UMWAIT spin-wait loop](https://stackoverflow.com/questions/74956482/working-example-of-umonitor-umwait-based-assembly-asm-spin-wait-loops-as-a-rep#)

