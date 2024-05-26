+++
title = "Inter Core Communication Pt 2: Queues and SeqLock Vectors"
date = 2024-05-26
description = "A deep dive on the components that comprise the inter core communication layer in Mantra: Seqlocks, message Queues and SeqlockedVectors"
[taxonomies]
tags =  ["mantra", "icc", "seqlock"]
+++
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
