+++
title = "Automatic Message Tracking and Timing"
date = 2025-01-01
description = "How Mantra automatically tracks and times each message."
[taxonomies]
tags =  ["mantra", "telemetry"]
[extra]
comment = true
+++
Happy new year, all!

In this post we will take a slight deviation away from the low level details of implementing a message passing distributed system.
Instead, we focus on a concrete application that the low-latency message `Queues` we implemented [in the previous post](@/posts/icc_2_queues_vectors/index.md) allow for.

# Overview/Context
As I have mentioned before, one of the main points of the `Queues` is that `Consumers` have absolutely no impact on `Producers` and other `Consumers`.
That means that we can attach auxilliary systems without impacting the main system's functionaly if we wanted to observe how it is behaving.
This is particularly useful for gathering telemetry, provided that the messages that are going through the system are slightly augmented with some additional metadata.
While the main system will therefore have to perform a bit more work, the real-world performance metrics and debugging capabilities that result from it are well worth the cost.
In fact, after having implemented the design below, I found that the overhead was so minimal that I forewent the planned feature flag disabling of the tracking.

Moving on, the main telemetry metrics I was interested in are:
- message propagation latency: how long does it take for downstream messages to arrive at different parts of the system based on an ingested message
- message processing time: how long does it take for message of type `T` to be processed by system `X`
- what are the downstream message produced by a given ingested message

This post will detail the message tracking design in **Mantra** to handle all of this as seemlessly as possible.

# `TrackingTimestamp` and `QueueMessage`
As mentioned before, we need to slightly "dress" raw data messages in order to track them and produce the desired telementry.
Remembering the lessons learned in previous posts, our main tool will again be the `rdtscp` cpu cycle counter.
This does mean that we only care about on the **same machine** message tracking.
If messages need to be tracked across machines, there is no way around using an actual timestamp (and potentially some sort of global UUID between machines).

The piece of metadata that is attached to the raw data messages is a `TrackingTimestamp`:
```rust
#[derive(Debug, Copy, Clone, PartialEq, Serialize, Default, Deserialize)]
pub struct TrackingTimestamp {
    pub origin_t:  OriginTime,  //basically the rdtscp count when the initial message was ingested by the system
    pub publish_t: PublishTime, //when was this message published to a Queue
}
```
The `publish_t` allows to track how long a message was pending in a `Queue` before it was picked up by the `Consumer`, and to easily reconstruct a timeline after a run of the system.

All messages going through the system will thus be `QueueMessages`:
```rust
#[derive(Default, Debug, Clone, Copy, Serialize, Deserialize)]
pub struct QueueMessage<T> {
    pub timestamp: TrackingTimestamp,
    pub data:      T,
}
```

# `Actor`, `Spine` and `SpineAdapters`
Now, it becomes extremely tedious and ugly if each of the `Producers` and `Consumers` have to take care of unpacking the `data`, process it, and then produce a new `QueueMessage` with the correct `origin_t` and `publish_t`, while also publishing the timing telemetry to the right timing queues.
Instead, I designed **Mantra** in such a way that all of this is handled behind the scenes, and sub-systems can just take care of their business logic.

We start by defining an `Actor` trait which is implemented by each sub-system. An `Actor` has a `name` which is used to create timing queues, a `loop_body` implementing the business logic, and potentially the `on_init` and `on_exit` functions which are called before the main `Actor` loop starts and after it finishes, respectively.

The second piece of the puzzle is the `Spine` which, as the name suggests, forms the backbone of the entire system, grouping all the different message queues:
```rust
pub struct Spine {
    pub msgs1: Queue<QueueMessage<MsgType1>>,
    pub msgs2: Queue<QueueMessage<MsgType2>>,
}
```

Each of the `Actors` is then given a `SpineAdapter` to `produce` and `consume` messages with:
```rust
#[derive(Clone, Copy, Debug)]
pub struct SpineAdapter {
    pub consumers: SpineConsumers,
    pub producers: SpineProducers,
}

#[derive(Clone, Copy, Debug)]
pub struct SpineProducers {
    pub msgs1:     Producer<QueueMessage<MsgType1>>,
    pub msgs2:     Producer<QueueMessage<MsgType2>>,
    pub timestamp: TrackingTimestamp,
}

#[derive(Clone, Copy, Debug)]
pub struct SpineConsumers {
    pub msgs1: Consumer<QueueMessage<MsgType1>>,
    pub msgs2: Consumer<QueueMessage<MsgType2>>,
}
```

This looks a bit convoluted, but it is this combined `SpineAdapter` structure that enables the core functionality: when the `SpineConsumers` consume a message,
the `timestamp` of that message is set on the `SpineProducers`, which is then attached to whatever message that the `Actor` produces based on the consumed one.
It completely solves the first issue of manually having to unpack and repack each message.

The second part is the automatic latency and processing time tracking of the messages. To enable this, we define a slightly augmented `Consumer` that holds a [`Timer`](@/posts/icc_1_seqlock/index.md#timing-101):

```rust
#[derive(Clone, Copy, Debug)]
pub struct Consumer<T: 'static + Copy + Default> {
    timer:    Timer,
    consumer: ma_communication::Consumer<QueueMessage<T>>,
}
```
These structs hold all the information to enable the api we want to end up with in the sub systems:
```rust
fn loop_body(&mut self, adapter: &mut SpineAdapter) {

    adapter.consume(|msg: MsgType1, producers: &SpineProducers|{
        // handle msg type 1
    });
    adapter.consume(|msg: MsgType2, producers: &SpineProducers|{
        // handle msg type 2
    });
}
```
with the latencies and processing times automatically tracked in timing queues.

The additional implementation needed looks like (some further implementation details are left to the reader):
```rust
// Initialization
impl SpineAdapter {
    pub fn attach<AC: Actor>(actor: &AC, spine: &Spine) -> Self {
        Self {
            consumers: SpineConsumers::attach(actor, spine),
            producers: SpineProducers::attach(actor, spine),
        }
    }
}

impl SpineProducers {
    pub fn attach(spine: &Spine) -> Self {
        Self {
            msgs1: Producer::from(spine),
            msgs2: Producer::from(spine),
            timestamp: TrackingTimestamp::default(),
        }
    }
}

impl SpineConsumers {
    pub fn attach<AC: Actor>(actor: &AC, spine: &Spine) -> Self {
        Self {
            msgs1: Consumer::attach(actor, spine),
            msgs2: Consumer::attach(actor, spine),
        }
    }
}

impl<T: 'static + Copy + Default> Consumer<T> {
    pub fn attach<A: Actor>(actor: &A, spine: &Spine) -> Self
    where
        Queue<T>: for<'a> From<&'a Spine>,
    {
        let timer = Timer::new(format!(
            "{}-{}",
            actor.name(),
            crate::utils::last_part(std::any::type_name::<T>())
        ));
        let q: Queue<T> = spine.into();
        Self {
            timer,
            consumer: ma_communication::Consumer::from(q),
        }
    }
}

// Consuming
impl SpineAdapter {
    #[inline]
    pub fn consume<T: 'static + Copy + Default, F>(&mut self, mut f: F)
    where
        SpineConsumers: AsMut<Consumer<T>>,
        F: FnMut(T, &SpineProducers),
    {
        let consumer = self.consumers.as_mut();
        // Choice here is to always consume all messages of a given queue, could be changed to be only a single message
        while consumer.consume(&mut self.producers, &mut f) {}
    }
}

impl<T: 'static + Copy + Default> Consumer<T> {
    #[inline]
    pub fn consume<F>(&mut self, producers: &mut SpineProducers, mut f: F) -> bool
    where
        F: FnMut(T, &SpineProducers),
    {
        self.consumer.consume(|&mut m| {
            producers.timestamp = m.timestamp;
            self.timer.start();
            f(m.data, producers);
            // Report processing time and latency after having processed message.
            // This minimizes the impact of telemetry on latency
            self.timer.stop_and_latency(m.timestamp.origin_t().into())
        })
    }
}

// Producing
impl SpineProducers {
    pub fn produce<T: Copy>(&self, d: &T)
    where
        Self: AsRef<Producer<T>>,
    {
        let msg = QueueMessage {
            timestamp: self.timestamp.set_publish_t(),
            data:      *d,
        };
        self.as_ref().produce(&msg);
    }
}

impl SpineAdapter {
    pub fn produce<T: Copy>(&mut self, d: &T)
    where
        SpineProducers: AsRef<Producer<T>>,
    {
        self.producers.produce(d);
    }

    pub fn set_origin_t(&mut self, origin_t: OriginTime) {
        self.producers.timestamp.origin_t = origin_t
    }

}
```

# Message Tracking and Persistence
Another sideffect of our implementation of `TrackingTimestamp` with `OriginTime` based on `rdtscp` and the fact that it is automatically attached to downstream messages
is that it effectively allows to create a "message tree".
After all, `rdtscp` can be used as a poor man's unique identifier, tying parent and offspring messages together.

Furthermore, since we now have all `Queues` grouped in a single `Spine`, it is very trivial to attach an `Actor` that takes care of persisting each and every message that flows through the system.
Given that we have the `OriginTime` and `PublishTime` of each message, we can reconstruct a timeline of the entire system's running. If we also store the produced timing messages, we get even more information on the system's performance: using the `consume` timestamp with `PublishTime` would allow to measure exactly the processing time of `MsgTypeX` -> `MsgTypeY`, and how long a message sat inside a `Queue` before it was handled.
Finally, with a minimal amount of elbow grease, it is possible to use the top 16 bits of `PublishTime` to store a u16 `ActorId` in case multiple `Actors` produce to the same message type, to be able to disentangle the performance of each `Actor` separately.

# Performance
While I will forego benchmarking the overhead of this type of telemetry gathering, I can posit that it is very minimal. The main differences to an unobservable system are:
- each message is 16 bytes larger
- on ingestion we take a `rdtscp` timestamp with 6-9ns overhead
- 1 `rdtscp` before handling a consumed message
- 1 `rdtscp` when producing a message
- 1 `rdtscp` after handling a message, followed by producing a TimingMessage ~(6-9 + ~12)ns

# Conclusion
With this relatively simple implementation and a very minimal performance sacrifice we gain an incredible wealth of information and observability of the system.
Having this design as the fundamental backbone of **Mantra** has proven invaluable in developing more advanced functionality as it allows for effortless performance
tracking of new or updated parts, as well as very effective debugging.

This concludes this post. A tad lighter than previous ones but hopefully just as informative and intersting. See you next time!
