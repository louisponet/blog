+++
title = "Inter Core Communication Pt 2: Queues and SeqLock Vectors"
date = 2024-07-14
description = "Applying the Seqlock in Mantra: Queues and SeqlockVectors"
[taxonomies]
tags =  ["mantra", "icc", "seqlock", "mpmc", "queue"]
[extra]
comment = true
+++
Welcome back to the second of a series of posts detailing the inter core communication components used in [**Mantra**](@/posts/hello_world/index.md).

In the previous post I detailed the implementation and functionality of the `Seqlock`.
As mentioned then, it is the core building block for all inter core synchronization in **Mantra**.
In this second post, we will use it to construct the low-latency message `Queues` for event based communication, and `SeqlockVectors` for communicating the "latest" information.
One example of the latter are the "ideal positions" that algo models produce based on incoming market data.

The `SeqlockVectors` are certainly the most straightforward of the two datastructures, so let's start with those.

# SeqlockVector

As the name suggests, a `SeqlockVector` is really not much more than a contiguous buffer of `Seqlocks`.
Each `[Version, Data, Padding]` triple in the image below  denotes one `Seqlock`:

![](SeqlockVector.svg#noborder "SeqlockVector")
*Fig 1. SeqlockVector*

Our implementation allows constructing them in the private memory of a given process as well as in a piece of shared memory created by the OS.

## SeqlockVectorHeader
To maximize the `SeqlockVector's` flexibility we also want it to describe itself to some degree by using the following `SeqlockVectorHeader` structure (shown in blue in the above figure):

```rust
#[derive(Debug)]
#[repr(C)]
pub struct SeqlockVectorHeader {
    elsize: usize,
    bufsize: usize
}
```
A bit underwhelming, perhaps. Nonetheless, it contains the information needed to allow a process to read the `SeqlockHeader` from shared memory, and know the number of bytes comprising each element (`elsize`), the number of elements (`bufsize`),
and thus the total bytesize of the `SeqlockVector`:
```rust
const fn size_of(bufsize: usize) -> usize {
    std::mem::size_of::<SeqlockVectorHeader>()
        + bufsize * std::mem::size_of::<Seqlock<T>>()
}
```

We use `[repr(C)]` to stop the compiler from reordering the struct fields.
This is mainly useful when accessing the `SeqlockVector` in shared memory by programs implemented in programming languages other than rust.
> Theoretically fields can also be ordered differently by rust compilers with different versions.

## Initialization
The `SeqlockVector` and its initialization are implemented as follows:
```rust,linenos, hl_lines= 40 53 68 79
use std::mem::{size_of, forget};
use std::ptr::slice_from_raw_parts_mut;
#[repr(C)]
pub struct SeqlockVector<T: Copy> {
    header: VectorHeader,
    buffer: [Seqlock<T>],
}
impl<T: Copy> SeqlockVector<T> {
    const fn size_of(bufsize: usize) -> usize {
        size_of::<SeqlockVectorHeader>()
            + bufsize * size_of::<Seqlock<T>>()
    }

    pub fn private(bufsize: usize) -> &'static Self {
        let size = Self::size_of(bufsize);
        unsafe {
            let ptr = std::alloc::alloc_zeroed(
                Layout::array::<u8>(size)
                    .unwrap()
                    .align_to(64)
                    .unwrap()
                    .pad_to_align(),
            );
            Self::from_uninitialized_ptr(ptr, bufsize)
        }
    }

    pub fn shared<P: AsRef<std::path::Path>>(
        shmem_flink: P,
        bufsize: usize,
    ) -> Result<&'static Self, &'static str> {
        use shared_memory::{ShmemConf, ShmemError};
        match ShmemConf::new()
            .size(Self::size_of(bufsize))
            .flink(&shmem_flink)
            .create()
        {
            Ok(shmem) => {
                let ptr = shmem.as_ptr();
                forget(shmem);
                Ok(Self::from_uninitialized_ptr(ptr, bufsize))
            }
            Err(ShmemError::LinkExists) => {
                let shmem = ShmemConf::new().flink(shmem_flink).open().unwrap();
                let ptr = shmem.as_ptr() as *mut VectorHeader;


                let v = Self::from_initialized_ptr(ptr);
                if v.header.bufsize < bufsize {
                    Err("Existing shmem too small")
                } else {
                    v.header.bufsize = bufsize;
                    forget(shmem); // shared_memory will be cleaned up when `Dropping`, so we explicitely leak it here.
                    Ok(v)
                }
            }
            Err(_) => {
                Err("Unable to create or open shmem flink.")
            }
        }
    }

    pub fn from_uninitialized_ptr(
        ptr: *mut u8,
        bufsize: usize,
    ) -> &'static Self {
        unsafe {
            let q = &*(slice_from_raw_parts_mut(ptr, bufsize) as *const SeqlockVector<T>);
            let elsize = size_of::<SeqLock<T>>();
            q.header.bufsize = bufsize;
            q.header.elsize = elsize;
            q
        }
    }

    fn from_initialized_ptr(ptr: *mut VectorHeader) -> &'static Self {
        unsafe {
            let bufsize = (*ptr).bufsize;
            &*(slice_from_raw_parts_mut(ptr, bufsize) as *const SeqlockVector<T>)
        }
    }

    pub fn len(&self) -> usize {
        self.header.bufsize
    }
}
```
The total size of a `SeqlockVector<T>` is given by the `size_of` function, `private` creates a new `SeqlockVector` in private memory only, and `shared` creates or opens a `SeqlockVector` in shared memory.
For the latter functionality we use the [`shared_memory`](https://docs.rs/shared_memory/latest/shared_memory/) crate.

I have highlighted the notably tricky parts of the code. The first two make sure that opened shared memory does not get automatically cleaned up when `shmem` gets dropped by using `forget`, while the last two create a reference to the `SeqlockedVector` from the raw shared memory pointer.

{{ note(header="Note!", body="Even though `slice_from_raw_parts_mut` is used to create a reference to the whole `SeqlockVector`, the `length` argument denotes the length of the unsized `buffer` slice only!") }}

## Read/write
The `read` and `write` implementations of the `SeqlockVector` are straightforwardly based on those of the [`Seqlock`](@/posts/icc_1_seqlock/index.md#tl-dr). See the code [here](https://github.com/louisponet/blog/blob/cd94640d5b372ae7cccfcae3bd366929284897a2/content/posts/icc_2_queues_vectors/code/src/vector.rs#L64-L106).

# SPMC/MPMC Message Queues
Without skippin a beat, we continue with the second datastructure that plays an even bigger role in **Mantra**.
Low-latency single/multi producer multi consumer message `Queues` form the basis for the modular and robust architecture of **Mantra**.

The underlying structure is pretty much identical to the `SeqlockVector`, i.e. a `buffer` of `Seqlocks` with the actual data, and a `QueueHeader` with metadata about the `Queue`.
As will become clear in due course, the `buffer` in this case is used as a ringbuffer.

```rust
#[repr(u8)]
pub enum QueueType {
    Unknown,
    MPMC,
    SPMC,
}
#[repr(C)]
pub struct QueueHeader {
    pub queue_type:         QueueType,   // 1
    pub is_initialized:     u8,          // 2
    _pad1:                  [u8; 6]      // 8
    pub elsize:             usize,       // 16
    mask:                   usize,       // 24
    pub count:              AtomicUsize, // 32
}

#[repr(C)]
pub struct Queue<T> {
    pub header: QueueHeader,
    buffer:     [seqlock::SeqLock<T>],
}
```

Before continuing with the `Producer` and `Consumer` implementations, let me [reiterate](@/posts/hello_world/index.md#queue) that `Producers` are oblivious to the attached `Consumers`, leading to a couple clear benefits:
- improves performance through less shared data and thus inter-core communication
- greatly simplifying the implementation
- a single misbehaving `Consumer` does not halt the entire system
- attaching more `Consumers` to a `Queue` is trivial as it does not impacting the performance or functionality of the rest of the system

One obvious negative is that `Consumers` can get sped past by `Producers`, leading to data loss from dropped messages.

Luckily, this happens extremely infrequently in reality and all observed cases were easily remedied by optimizing the code or offloading to another `Consumer` on an additional cores.
The points outlined above mean that even the latter, more dramatic case, is easily implementable.

Nonetheless, we at least want `Consumers` to be able to autonomously detect when they are sped past.
As we will demonstrate, this can be achieved even though the only shared data between `Consumers` and `Producers` is the `buffer` of `Seqlocks`.

One final remark before continuing with the implemenation details is that all `Consumers` observe all messages flowing through a given `Queue`. This is sometimes referred to as `broadcast` or `multicast` mode.
## `Producer`
```rust,linenos, hl_lines= 26
impl<T: Copy> Queue<T> {

    pub fn len(&self) -> usize {
        self.header.mask + 1
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
    fn load(&self, pos: usize) -> &SeqLock<T> {
        unsafe { self.buffer.get_unchecked(pos) }
    }

    fn produce(&self, item: &T) -> usize {
        let p = self.next_count();
        let lock = self.load(p & self.header.mask);
        lock.write(item);
        p
    }

}
```

The `count` field is used to keep track of the position of the next free slot to write a message into.

The `mask` is used to make sure that the `Producer` loops back to the start of the `Queue` when it reaches the end of the `buffer`:
if we only allow `Queue` sizes that are a power of two and set `mask` equal to `busize - 1`, the `p & self.header.mask` in line 21 is the same as `p % bufsize`.
Let's try to understand this with a minimal example:
```rust
let bufsize = 8;
let mask = 8 - 1;            // 7
let p = 7 & mask;            // 0x...0111 & 0x...0111 = 0x...0111 = 7
let p_next = (p + 1) & mask; // 0x...1000 & 0x...0111 = 0x...0000 = 0
```
Very clever. Unfortunately I can not claim authorship of this rather well known trick, alas. It also avoids using
```rust
if (self.header.count == self.len()) {
    self.header.count = 0
} else {
    self.header.count += 1 //or fetch_add
}
```
in `next_count`, which would inevitably lead to branch prediction misses.

Line 10 shows the only difference between the single and multi `Producer` cases.
`count` is shared between all of the `Producers`, and we have to make sure no two `Producers` try to write to the same slot at the same time.
By using `fetch_add` a `Producer` can both reserve a slot for his message, as well as advance the counter for other `Producers`.
> The only potential problem is that if another `Producer` manages to make it all the way around the `buffer` to the same slot, in the time that the first `Producer` is still writing the message. Then they might be writing to the same data at the same time. In reality it's clear that this never happens


## `Consumer` implementation
As mentioned before, `Consumers` are be implemented such that only the `Seqlock` buffer is shared with other `Consumers` and `Producers`.
We thus have to figure out a way for them to understand when the next message is ready, and wether they got sped past by a `producer`.

The `version` of the `SeqLocks` can be used for both given the following observations:
1. the next message to be read will be in the slot whose `version` is lower than that of the previous slot
2. the version of the slot containing the next message to read will be exactly 2 more than before once the message is ready
3. the version of the `Seqlocks` can be fully deduced from the `count` of the queue: `(count / bufsize)` is how many times all `Seqlocks` have been written to and `count % bufsize` is the position of the next `Seqlock` to be written to

Let's then first initialize a `Consumer` with the current `count` and the expected `version` of the `Seqlocks` holding messages to be read:
```rust
#[repr(C)]
#[derive(Debug)]
pub struct Consumer<'a, T> {
    pos:              usize, // 8
    mask:             usize, // 16
    expected_version: usize, // 24
    queue:            &'a Queue<T>, // 48 fat ptr: (usize, pointer)
}

impl<'a, T: Copy> From<&'a Queue<T>> for Consumer<'a, T> {
    fn from(queue: &'a Queue<T>) -> Self {
        let mask = queue.header.mask;
        let c = queue.header.count.load(std::sync::atomic::Ordering::Relaxed);
        let pos = c & queue.header.mask;
        /* e.g.
            seqlock_versions = [4, 2, 2, 2, 2, 2, 2, 2]
                                   ^ next message, ready when version switches to 4
            len = 8
            count = 9
            count / len = 9 / 8 = 1
            1 << 1 = 2
            expected_version = 2 + 2 = 4
        */
        let expected_version = ((queue.header.count / (queue.len())) << 1) + 2;
        Self {
            pos,
            mask,
            expected_version,
            queue,
        }
    }
}
```

A `Consumer` knows when the next message is ready to be read when the `Seqlock` `version` jumps to the `expected_version`, i.e. when a `Producer` has finished writing it.

A `Consumer` can also deduce when it is sped past when it sees that the `version` of the next message it tries to read is higher than the `expected_version`.
The only way that this is posible is if a `Producer` wrote to the `Seqlock`, thereby incrementing the `version` to the expected one, then looping all the way around the `buffer` back to the same `Seqlock` and writing another message, thus incrementing the `version` to one higher than `expected_version`.
When the slow `Consumer` finally comes along it will have lost the data of the overwritten message because it was sped past.
At least with this implementation we know and choose how to deal with it.

A visual representation of the possible scenarios is shown in the following figure:

![](Queue.svg#noborder "Queue flow")

*Fig. 2: The operation flow of a `Producer` and `Consumer`*

We start by implementing a new `read_with_version` function on the `Seqlock`:
```rust
use thiserror::Error;

#[derive(Error, Debug, Copy, Clone, PartialEq)]
pub enum ReadError {
    #[error("Got sped past")]
    SpedPast,
    #[error("Queue empty")]
    Empty,
}

#[inline(never)]
pub fn read_with_version(
    &self,
    result: &mut T,
    expected_version: usize,
) -> Result<(), ReadError> {
    loop {
        let v1 = self.version.load(Ordering::Acquire);
        if v1 != expected_version {
            if v1 < expected_version {
                return Err(ReadError::Empty);
            } else {
                return Err(ReadError::SpedPast);
            }
        }

        compiler_fence(Ordering::AcqRel);
        *result = unsafe { *self.data.get() };
        compiler_fence(Ordering::AcqRel);
        let v2 = self.version.load(Ordering::Acquire);
        if v1 == v2 {
            return Ok(());
        }
    }
}
```
The main difference to the previously discussed [`read`](@/posts/icc_1_seqlock/index.md#tl-dr) function is that we now know what `version` the `Consumer` expects.
By returning `Result<(), ReadError>`, the current situation is communicated back to the caller: either a message has succesfully been read (`Ok(())`), no message is ready yet (`Err(ReadError::Empty)`), or the `Consumer` was sped past (`Err(ReadError::SpedPast)`).

We implement the `try_read` function for the `Consumer` based on this:
```rust
fn update_pos(&mut self) {
    self.pos = (self.pos + 1) & self.mask;
    self.expected_version += 2 * (self.pos == 0) as usize;
}

/// Nonblocking consume returning either Ok(()) or a ReadError
pub fn try_consume(&mut self, el: &mut T) -> Result<(), ReadError> {
    self.queue.load(self.pos).read_with_version(el, self.pos, self.expected_version)?;
    self.update_pos();
    Ok(())
}
```

# Conclusion
Boom, that's it for this installment folks!
We have covered the main concepts behind how the `SeqlockVector` and `Queue` synchronization datastructures have been implemented in **Mantra**.
These form the backbone that allow all systems to work together. We have implemented using the `Seqlocks` we have discussed in the previous post, and made them as self-sufficient as possible. As highlighted before, this helps with performance and overall robustness of the system. 

Next time we will go about thorougly testing, benchmarking, and potentially optimizing the implementation, hope to see you there!.
