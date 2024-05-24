+++
title = "Inter Core Communication"
date = 2024-05-14
description = "A deep dive on the components that comprise the inter core communication layer in Mantra: Seqlocks, message Queues and SeqlockedVectors"
[taxonomies]
tags =  ["mantra", "icc", "seqlock"]
+++

In this first technical blog post we will take a closer look at some inter core communication and data synchronization components that form the most fundamental layer of [**Mantra**](@/posts/hello_world/index.md).

I have taken a great deal of inspiration from:
- [Trading at light speed](https://www.youtube.com/watch?v=8uAW5FQtcvE) by David Gross
- An amazing set of references: [Awesome Lockfree](https://github.com/rigtorp/awesome-lockfree) by Erik Rigtorp
# Design Goals and Considerations

- Achieve a close to the ideal ~30ns core-to-core latency (see e.g. [anandtech 13900k and 13600k review](https://www.anandtech.com/show/17601/intel-core-i9-13900k-and-i5-13600k-review/5) and the [fantastic core-to-core-latency tool](https://github.com/nviennot/core-to-core-latency))
- _Producers_ are favored, i.e. they do not care about and are not impacted by data _Consumers_
- _Consumers_ should not impact eachother, or the system as a whole 
- Every _Consumer_ gets access to all data, in the case of message _Queues_ this is also known as *broadcast* mode

We go into further detail on how each of these points impacted the implementation of the various components.

# Seqlock
Considering the above goals, the `Seqlock` (see [Wikipedia](https://en.wikipedia.org/wiki/Seqlock), [seqlock in the linux kernel](https://docs.kernel.org/locking/seqlock.html), and [Erik Rigtorp's C++11 implementation](https://github.com/rigtorp/Seqlock))

Rather than regurgitating the same insights as in these stellar references, let me break it down to the essentials:
- A _Producer_ (or writer) is never blocked by _Consumers_ (readers)
- The _Producer_ atomically increments a counter (the _sequence_) once before and once after writing the data
- `counter & 1 == 0` (even) communicates to _Consumers_ that they can read data
- `counter_before_read == counter_after_read`: data was read consistently
- Compare and swap can be used on the counter to allow multiple _Producers_ to write to same _SeqLock_ (we won't use it, later more on why)
- Depending on the architecture and compiler, it's crucial to verify that the sequence of operations are not reordered: memory barriers/fences are usually required

Let's try to design some tests to validate a _SeqLock_ implementation, focusing mainly on the last point.

## Naive starting point
```rust,linenos
use std::sync::atomic::{AtomicUsize, Ordering};
use std::cell::UnsafeCell;

pub struct SeqLock<T> {
    version: AtomicUsize,
    data: UnsafeCell<T>,
}

impl<T: Copy> SeqLock<T> {
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
}
```
Before discussing what may or may not be wrong with this implementation, let's design some tests to verify that it does what we think it does.
We use a fixed size array `msg` of different sizes as our message. We have one `Consumer` thread who does the actual data verification, and one `Producer` thread that continuously writes new data by incrementing `count` and filling `msg` with it:
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
                        assert_eq!(first, i); // data consistency is verified here
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
Data consistency (i.e. no partially written data was read) is tested by verifying that all elements in `msg` are the same as the first.
On my intel i9 14900k these tests fail for array sizes of 64 and up (512 bytes). This signals that either the compiler or the cpu did some reordering of operations.
In fact, on x86 it can only be due to reordering by the compiler (see the [Release-Acquire ordering paragraph in the c++ reference](https://en.cppreference.com/w/cpp/atomic/memory_order#Release-Acquire_ordering)).

In this case the issue is that we allow the compiler to inline our `read` function and it actually reorders the `let first = msg[0]` line to be executed before
the `read`, causing obvious problems. Just to highlight how fickle inlining is, adding `assert_ne!(msg[0], 0)` at the very end of the `Consumer` function makes all tests pass.

One solution is to make either `self.version.load` functions use the `Ordering::Acquire` since that forces the compiler to not reorder the sequence of operations accross this barrier.
For now we just add `#[inline(never)]` to the `read` function, and it is best we do the same for the `write` function. This minimizes the "compiler-handholding" surface area.

## Deeper dive using [`cargo asm`](https://crates.io/crates/cargo-show-asm/0.2.34)
This is for sure one of the best tools for this kind of analysis. I highly recommend adding `#[inline(never)]` to any function you are planning to analyze.

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
For now this is fine, but in the future we will annotate the struct with `#[repr(C)]` to at least keep field order.

From the point of view of the `SeqLock` implementation, the important lines are highlighted. These correspond pretty much exactly with the `read` function:
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
Well at least it looks clean... I'm pretty sure I don't have to underline the issue with
1. Do the copy into `rax`
2. move the **exact same version** into `rcx` and `rdx`
3. test like before
4. copy from `rax` into the input
5. No Stonks...

My earlier tests clearly didn't catch everything, in fact I never got them to fail after adding the `#[inline(never)]`.
I think the reason is that for small enough data, the `memcpy` is done **inline/in cache** with moves into and out of different registers (`rax` in this case) and
that it is extremely unlikely that the cache gets invalidated/overwritten during these operations.

### Adding Memory Barriers
In any case, I guess it's time to add some barriers to stop the compiler from shooting us in the foot:
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
It is funny that the compiler chooses to reuse `rcx` both for the data copy in lines `(5, 6)`, as well as the second version load in line `8`.

With the current `rust` compiler (1.78.0), adding either `Ordering::Acquire` in lines `4` or `7` does the trick.
However, technically they only influence the ordering of loads of the `version` Atomic when combined with an `Ordering::Release` store in the `write` function. The compiler is still theoretically allowed to reorder the actual copying of the data.
The compiler fences should forever block this reordering, and on x86 that should be enough (again, see the [Release-Acquire ordering paragraph in the c++ reference](https://en.cppreference.com/w/cpp/atomic/memory_order#Release-Acquire_ordering)).

Bearing these things in mind, we add the corresponding barriers to the `write` function:
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
For our purposes this should do the trick, and we will now move on to timing and potentially optimizing our implementation. However, if memory models and barriers are really your schtick, live a little and marvel your way through [The Linux Kernel Docs on Memory Barriers](https://docs.kernel.org/core-api/wrappers/memory-barriers.html).

# Performance
Now that the implementation behaves correctly, we can finally do some juicy performance measuring.

## Timing 101
The full details regarding the suite of timing and performance measurements tools I have developed to track the performance of **Mantra** will be divulged in a later post.
For the purposes here the main points are:

**Use `rdtscp` to take timestamps**: the `rdtscp` hardware counter in most recent cpus is a monotonously increasing cpu cycle counter (at base frequency) which is reset upon startup. What's even better is that on recent cpus it is shared between all cores (look for `constant_tsc` in `/proc/cpuinfo`). It is the cheapest, at ~5ns overhead, and most precise way to take timestamps. An added benefit is that it partially orders operations (see discussion above), in that it will not execute until _"all previous instructions have executed and all previous loads are globally visible"_ see [this](https://www.felixcloutier.com/x86/rdtscp). Using an `_mm_lfence` after the initial `rdtscp` will also force executions to not begin before the timestamp is taken. This is **the only reasonable way** to time on really low latency scales.

**use [`core_affinity`](https://docs.rs/core_affinity/latest/core_affinity/) and `isolcpus`**: The combination of the [`isolcpus`](https://wiki.linuxfoundation.org/realtime/documentation/howto/tools/cpu-partitioning/isolcpus) kernel parameter with binding a thread in `rust` to a specific core allows us to minimize jitter coming from whatever else is running on the computer. I have isolated performance cpus 0-9 for testing purposes.

**Offload the actual timing**: To minimize the timing overhead we take the two `rdtscp` stamps and offload them to a `Queue` in shared memory (more on what a `Queue` is later). Another process can then read these messages, collect statistics and convert `rdtscp` stamp deltas to nanoseconds (in the i9 14900k case x3.2). For this last step we can actually reuse the [`nanos_from_raw_delta`](https://docs.rs/quanta/latest/quanta/struct.Clock.html#method.delta_as_nanos) from the `quanta` library.


Putting all of this together, timing a block of code behind the scenes essentially looks like:
```rust
let t1 = unsafe { __rdtscp(&mut 0u32 as *mut _) };
unsafe { _mm_lfence() };
// code to be timed
let t2 = unsafe { __rdtscp(&mut 0u32 as *mut _) };
timer_queue.produce((t1, t2));
```
using my timing library,
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
This will come in handy in our performance measurements. The former will be called `Business` timing (for business logic), and the latter, you guessed it, `Latency` timing.

The `timekeeper` tui then picks up on these timers and will display a continuously updated graph of timings (take a moment to familiarise yourself):

![](timekeeper_example.png#noborder "ui")

## Baseline Inter Core Latency

# SPMC/MPMC Message Queues
# Seqlocked Buffers
