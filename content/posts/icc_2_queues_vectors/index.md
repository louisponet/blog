+++
title = "Inter Core Communication Pt 2: Queues and SeqLock Vectors"
date = 2024-07-13
description = "Applying the Seqlock in Mantra: Queues and SeqlockVectors"
[taxonomies]
tags =  ["mantra", "icc", "seqlock", "mpmc", "queue"]
[extra]
comment = true
+++
Welcome back to the second of a series of posts detailing the inter core communication components used in [**Mantra**](@/posts/hello_world/index.md).

In the previous posed we very meticulously studied the main primitive that facilitates synchronization between multiple cores: the `Seqlock`.
Here, I will continue by showcasing two of its applications: `Queues` for event based communication, and `SeqlockVectors` for communicating the "latest" information.
One example of this latter, as outline before in the overview of [**Mantra's** Architecture](@/posts/hello_world/index.md#Architecture), are the "ideal positions" that algo models produce based on incoming market data.

Given that `SeqlockVectors` are the most straightforward of the two datastructures, we will look at these first.

# SeqlockVector

As the name suggests, a `SeqlockVector` is really not much more than a contiguous buffer of `Seqlocks`.
The way that they are implemented below allows for them to be constructed either in the private memory of a process, i.e. allocated by the global `rust` allocator, or in a piece of shared memory created by the OS.

## SeqlockVectorHeader
To maximize the `SeqlockVector's` flexibility we also want it to describe itself to some degree.

This can be achived by preceding the buffer of `Seqlocks` with the following `SeqlockVectorHeader` structure:
```rust
#[derive(Debug)]
#[repr(C)]
pub struct SeqlockVectorHeader {
    elsize: usize,
    bufsize: usize
}
```
While it might not seem like much, this allows a process to read the `SeqlockHeader` from shared memory, and know the number of bytes comprising each element (`elsize`), the number of elements (`bufsize`),
and thus the total bytesize of the `SeqlockVector`: `elsize * bufsize + std::mem::size_of::<VectorHeader>()`. We use `[repr(C)]` again to make sure that the compiler does not reorder the struct fields.
This is mainly useful when accessing the `SeqlockVector` in shared memory by programs implemented in programming languages other than rust (although technically fields could be ordered differently by rust compilers with different versions).

## Initialization
The `SeqlockVector` itself, and its initialization is implemented as:
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
The total size of a `SeqlockVector<T>` is given by the `size_of` function, `private` creates a new `SeqlockVector` in private memory only, and `shared` creates a new, or opens an existing `SeqlockVector` in shared memory.
For the latter functionality we use the [`shared_memory`](https://docs.rs/shared_memory/latest/shared_memory/) crate.

The notably tricky parts of the code are highlighted. They mostly involve jumping through some hoops to make sure that opened shared memory does not get automatically cleaned up (i.e. by using `forget`), and how we can create a reference to the `SeqlockedVector` from the raw shared memory pointer.

{{ note(header="Note!", body="Even though `slice_from_raw_parts_mut` is used to create a reference to the whole `SeqlockVector`, the `length` argument denotes the length of the unsized `buffer` slice only!") }}

## Read/write
The `read` and `write` implementations of the `SeqlockVector` are straightforwardly based on those of the [`Seqlock`](@/posts/icc_1_seqlock/index.md#tl-dr). See the code [here](https://github.com/louisponet/blog/posts/icc_2_queues_vectors/code/).

```rust
fn load(&self, pos: usize) -> &SeqLock<T> {
    unsafe { self.buffer.get_unchecked(pos) }
}

fn pos_assert(&self, pos: usize) {
    assert!(pos < self.len(), "OutOfBounds: index {pos} larger than size {}", self.header.bufsize);
}

pub fn write_unchecked(&self, pos: usize, item: &T) {
    let lock = self.load(pos);
    lock.write(item);
}

pub fn write(&self, pos: usize, item: &T) {
    self.pos_assert(pos);
    self.write_unchecked(pos, item);
}

pub fn read_unchecked(&self, pos: usize, result: &mut T) {
    let lock = self.load(pos);
    lock.read_no_ver(result);
}

pub fn read(&self, pos: usize, result: &mut T) {
    self.pos_assert(pos);
    self.read_unchecked(pos, result)
}

pub fn read_copy_unchecked(&self, pos:usize) -> T {
    let mut out = unsafe {MaybeUninit::uninit().assume_init()};
    let lock = self.load(pos);
    lock.read_no_ver(&mut out);
    out
}
pub fn read_copy(&self, pos: usize) -> T {
    self.pos_assert(pos);
    self.read_copy_unchecked(pos)
}

```


# SPMC/MPMC Message Queues
Now that we have meticulously crafted, thoroughly tested and (possibly) fully optimized our `SeqLock` implementation, we can start using it in actually useful data structures.

The first is a message or event `Queue`, supporting both single-producer-multi-consumer and multi-producer-multi-consumer modes.
One thing to note here is that we always mean multi-consumer from a **broadcasting** pov, i.e. each `Consumer` sees and processes each message.
Again, `Producers` are oblivious to the attached `Consumers`. This improves performance, makes the implementation a lot simpler, but also means that a single bad `Consumer` does not kill the system + we can trivially "on the fly" attach more `Consumers`.

## Implementation Concept
The underlying datastructure consists of an array of `SeqLocks`, treated as a ringbuffer.
To make things fast, we require the size of the ringbuffer to be a power of two. This means that if we can keep track of the current message id with a single `counter`, the `produce` method can be branchless while avoiding the cost of a `mod` operation:

```rust
fn produce(&self, item: &T) -> usize {
    let p = self.counter;
    let lock = self.buffer[p & self.mask];
    lock.write(item);
}
```
with `mask` being `queue.len() - 1`. This is a well known trick, use it everywhere.

The next question is how do `Consumers` know what message to read next, and when they sped past.
We absolutely want to avoid them reading the main `counter` value each time, because that would require it to be continously shared
between all `Producers` and `Consumers`.

Instead, we can use the `version` of the `SeqLocks` by realizing that:
1. the next message to be read will be in the slot whose `version` is lower than that of the previous slot
2. the version of the slot containing the next message to read will be exactly 2 more than before once the message is ready

If the `Consumers` track an `expected_version` together with their own `count`, they can know exactly which message to read next and
whether they got sped past (they find a message at their next position that has too high a version).

The flow shown below should make the idea clear 

![](Queue.svg#noborder "Queue flow")



# Seqlocked Buffers
