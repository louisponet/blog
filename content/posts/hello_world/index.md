+++
title = "Hello World, this is Mantra"
date = 2024-05-24
description = "The current status and design overview of my hft trading engine, and an outline of the planned discussion topics related to it."
[taxonomies]
tags =  ["mantra", "hft", "icc"]
[extra]
comment = true
+++

I started work on **Mantra** in an effort to learn **rust** and explore the development process of a distributed, high(ish)-frequency/low-latency trading system in the language.
From the get go, I've targeted an internal [tick-to-trade](https://beeksgroup.com/blog/tick-to-trade-is-it-the-new-must-have-metric-in-trading-performance/) latency in the low microsecond range.
While this has forced me to thread carefully while adding an ever increasing amount of capabilities and features, I have been able to keep **Mantra's** internal latency firmly below this target.

I've now reached a point where I feel like some of the solutions and designs I came up with are worth sharing, hence this blog.
While **Mantra** itself will remain closed source, for obvious reasons, it will serve as a _fil rouge_ tying the concepts of deeper discussions together, and show how they can be applied to a real-world high-performance application.

As `rust` has been my language of choice for **Mantra**, many of the code snippets will be written in it.
The un-enlightened should not fret, however, since the focus will be on the concepts themselves and most of these should be quite straight-forwardly translatable into any capable programming language.

The intention of this initial blog post is to give an overview of the features and general design of **Mantra**, setting the stage for future more technical posts.

Enough with the intro, feature list time...
# Features and Capabilities
![](ui.png#noborder "ui")
***Mantra** ui during a backtest*

- Multicore
- **NO ASYNC**
- Low latency inter core communication using hand crafted message **queues, seqlocks, and shared memory**
- Full internal observability (message tracking) and high performance **in-situ telemetry** of latency and business logic
- Full **persistence of messages** for post execution analysis and replay
- [L2](https://centerpointsecurities.com/level-1-vs-level-2-market-data/) based orderbooks
- Concurrent handling of multiple trading algorithms
- Balance and order tracking accross multiple Exchanges
- Continuous **ingestion and storage** of market data streams
- WebSocket based connections to 5 crypto Exchanges (Binance, Bitstamp, Bitfinex, Coinbase and Kraken)
- "In production" **backtesting** by replaying historical market data streams while mimicking order execution with a **mock exchange**
- Real-time UI for observation of the system, market data and backtesting
- ~500k msgs/s throughput @ 0.5 - 10 microseconds internal latency
- A focus on code simplicity and **locality of behavior** (< 15K LOC for now)

# System Design Overview
The core design has changed very little since **Mantra's** inception.
Pragmatism has always been one of the guiding principles given the scope of creating a fully fledged and capable low-latency trading engine... while learning a new programming language.

This has led me to a very modular and disconnected design, allowing me to add features with minimal friction while also minimizing the impact of inevitable refactorings as I learned more about `rust`.
The potentially lower latency *single-function-hot-path* approach inevitably leads to more intertwined and hard to maintain code. I am also convinced that it is easier to be build the latter on top of a solid distributed system rather than the other way around.

As the schematic below shows, **Mantra** is thus composed of a set of `Actors` that communicate with eachother through lock-free `Queues`.
![](system_design.svg#noborder)
*Fig 1. High level design of **Mantra***

The main execution logic and data flow is quite straight-forward:
1. incoming `L2Update` and `TradeUpdate` market data messages (green, top-left) get consumed by the `TradeModels` (in grey)
2. each `TradeModel` fills out a pre-defined set of ideal order positions
3. the `Overseer` actor continuously loops through these, and compares them to previously sent `OrderRequests` and live `Orders` on the target `Exchange`
4. if they don't match up and the necessary `balance` is available on the `Exchange`, the `Overseer` generates and publishes a new `OrderRequest`
5. the `AccountHandler` connecting with the target `Exchange` then sends these requests
6. `Order`, `Balance` and `OrderExecution` updates are fed back `Overseer`

As will be discussed in future posts, centering the system around multi-consumer message `Queues` has many benefits.
They are set up to broadcast every message to every attached `Consumer`, and are designed such that `Consumers` do not impact eachother nor `Producers`.
This makes adding functionality through new `Actors` frictionless. Most will have realized by now that it's essentially a low-latency microservice design that actually works (one person team etc).

Another benefit is that functionality can be switched on or off at will. `Queues` use shared memory, meaning that these `Actors` could be running in different processes which is exactly how the **UI** and **telemetry** of **Mantra** work.

One final consideration against using a single-function-hot-path approach is that **Mantra** currently targets **crypto** markets.
While there is nothing particularly specific to **crypto** outside of the WebSocket connections, it does mean that the vast majority of latency (10s of milliseconds) actually originates from the connection between my pc and the exchanges.
I hope to be able to co-locate with some of them in the future at which point achieving the absolute minimal latency becomes more critical (target of ~10-100ns). Again, pragmatism...

Having raved about `Queues` this much, let me now give an overview of the the inter core communication layer.
## Inter Core Communication (ICC)

![](Queue.svg#noborder)
*Fig 2. Seqlocked Buffer*

The `Queues` are denoted in [Fig 1.](@/posts/hello_world/index.md#system-design-overview) by the red arrows and ovals that specify the message type of each `Queue`.
They are essentially [`Seqlocked`](https://en.wikipedia.org/wiki/Seqlock) ringbuffers that can be used both in *single-producer-multi-consumer* (SPMC) and *multi-producer-multi-consumer* (MPMC) modes.

Reiterating the main design considerations for their application in **Mantra**:
- Achieve a close to the ideal ~30ns core-to-core latency (see e.g. [anandtech 13900k and 13600k review](https://www.anandtech.com/show/17601/intel-core-i9-13900k-and-i5-13600k-review/5) and the [fantastic core-to-core-latency tool](https://github.com/nviennot/core-to-core-latency))
- Every attached `Consumer` gets every message, also known as *broadcast* mode
- `Producers` are "not" impacted by number of attached `Consumers` (difficult to achieve perfectly), mainly they don't care if `Consumers` can keep up
- `Consumers` should not impact eachother, and should know when they got sped past by `Producers`

As a result of the 3rd point, `Producers` and `Consumers` do not share any state other than the ringbuffer itself.
`Consumers` know which version of the `Seqlocks` that guard the data they expect.
That allows them to autonomously know when the next message is ready: a `Producer` has incremented the version of the next `Seqlock` to the one they expect.
If while running through the `Queue` they encounter a `Seqlock` with a version that is larger than what they expected they know they have been sped past: a `Producer` has written data and incremented the counter twice.

In **Mantra**, aside from the `Consumers` that handle the business logic, each `Queue` also has a `Consumer` that persists each message to disk. This allows for post-mortem analysis and replay.

The second part of the ICC layer are the `SeqlockVectors` denoted by the blue rectangles in [Fig 1.](@/posts/hello_world/index.md#system-design-overview).
They are for now only used between the `TradeModels` and the `Overseer`.
The reason I've used these rather than another `Queue` is because `TradeModels` generally recalculate their ideal positions on each incoming marketdata message.
If a `TradeModel` recomputed the values for a given ideal `Order` multiple times while the `Overseer` was busy, it would still have to go through the messages from oldest to newest.
However, we really only want to potentially send `Orders` based on the latest information. Using `SeqlockVectors` allows the `TradeModels` to overwrite the different `Orders` and the `Overseer` only reads the latest one for each.

I think that by now it is clear that the `Seqlock` is the main synchronization primitive that is used throughout **Mantra**, and the next blog post will be a deeper dive exactly on that.
While it is quite a well understood concept, and I will indeed reference much literature that gave me inspiration, I will focus on how one would verify and time the implementation, which I think is not often discussed.

## Telemetry and Observability

As mentioned in the introduction, I have put great emphasis on in-situ telemetry from the very beginning to keep the performance of different parts of **Mantra** in check while adding features.

Given the low-latency nature of **Mantra** I decided:
- To use the hardware timer [`rdtscp`](https://www.felixcloutier.com/x86/rdtscp) for timestamping: more accurate, less costly than OS timestamps
- That each message that enters the system gets an **origin** timestamp which is **propagated** to all downstream messages that result from it
- That when a message gets **published** to a `Queue` its time `delta` w.r.t. the **origin** timestamp is stored together with the publisher's `id`
- To offload these timestamps to specific timing `Queues` in **shared memory** so external tools can do the timing analysis

This scheme allows me to automatically time the different parts of **Mantra** with minimal overhead and to a high degree of accuracy.
It also allows to track the origin and "ancestry" of all messages in the system which is invaluable while debugging or optimizing.

Together with the small `timekeeper` tui tool and the fully fledged `ui` shown below, this methodical focus on observability has allowed me to make informed implementation decisions every step of the way.

![](timers.png#noborder)
*Fig 3. timekeeper tui and per `OrderRequest` message internal latency*

## Market Data
The final and arguably most important piece of the puzzle is market data.
I chose to focus on [L2](https://centerpointsecurities.com/level-1-vs-level-2-market-data/) orderbook data and trade execution data, since most exchanges readily provide those in the public domain.
For now this is streamed through **WebSockets**, although I plan to implement [FIX](https://www.investopedia.com/terms/f/financial-information-exchange.asp) exchanges that support it.

**Mantra** handles `L2OrderBooks` per `Instrument` per `Exchange`. They are implemented in a relatively standard way, using `BTreeMaps` for the `bid` and `ask` sides to facilitate easy insertion, removal and in-order scanning of price levels.

What is perhaps more interesting is that **Mantra** is continuously capturing and archiving all incoming `L2Update` and `TradeUpdate` messages for the 5 `Exchanges` I am currently connecting to: Binance, Bitfinex, Bitstamp, Coinbase and Kraken.
With "all" I mean literally all, i.e. the data for every single spot `Instrument` that is tradable on these exchanges.
This leads to quite a lot of data, so I use [`bitcode`](https://docs.rs/bitcode/latest/bitcode/#) together with [`zstd`](https://docs.rs/zstd/latest/zstd/#) to achieve quite impressive data compression ratios of ~20x.

Some quick numbers are:
- 169 simultaneous websocket connections
- 5764 `Instruments` accross 5 exchanges
- up to 200k msgs/s handled
- up to 47 Gb / day
- 2.9 Tb worth of data so far
- ~40% of 1 core used for processing at rates of 35k msgs/s
- **STILL NO ASYNC**

"But why gobble up all that data", you may wonder. Glad you asked.

I am of the very strong opinion that representative **backtests** should be performed as much as possible on the **"in-production"** system rather than through some idealized transformations of DataFrames (although that is perfect for initial strategy exploration).
Or assuming that different parts of the system take a fixed amount of time to execute.
This is especially true for *low-latency* systems, given how tightly coupled the performance and implementation of the system are with the trading algos and their parameters.

I have thus implemented a `MockExchange` which feeds the captured historic market data back into the system, and simultaneously uses it to mimic the behavior of a real `Exchange`.
Of course there are some approximations here, and backtests are not fully deterministic this way, it nonetheless provides a successful strategy for backtesting the system as a whole.

This gives only a very short glimpse into this relatively deep topic, and I have some interesting additional experiments planned for the multi series of blog posts on Market Data.

# Planned Blog Posts
Based on the overview the initially planned blog posts are:
1. **Low latency inter core/process communication**; Seqlocks, broadcasting message queues and synchronized arrays
2. **High performance in-situ telemetry and message tracking**; How every message is tracked and every part of **Mantra** is timed
3. **Market Data**; Large scale ingestion and storage, `MockExchange` and L2 Orderbooks
4. **UI**; A closer look at `egui` and how it can be used as a high performance timeseries analysis tool

# Planned Future Work on **Mantra**
- [ ] FIX connections
- [ ] Order queue position in the `MockExchange`
- [ ] Improved parsing of market data messages and direct NIC access
- [ ] More backtest performance metrics
- [ ] More advanced market data based signal generation
