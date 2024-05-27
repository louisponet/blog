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
One of the driving goals was to achieve a [tick-to-trade](https://beeksgroup.com/blog/tick-to-trade-is-it-the-new-must-have-metric-in-trading-performance/) internal latency on the order of 0.5 - 10$\mu$s (depending on trading algo complexity).

I've now reached a point where I feel like some of the solutions and designs I came up with are worth sharing. 
While most code snippets will be written in `rust`, the language is not the main focus, which should make translating the discussed concepts easily applicable using other languages.
I hope to also learn a thing or two through eventual discours with more seasoned practitioners, so be sure to make plentiful use of the *Comment* field!

**Mantra** itself will remain closed-source, for obvious reasons, but I will use this blog to go through and give detailed discussions on some of its core building blocks.
Most of these have been purposefully hand-crafted focusing on pragmatism given the scope of the project.

The intention of this initial blog post is to give an overview of the features and general design of **Mantra**, setting the stage for future more technical posts.

Let's start with a quick billboard rundown of the current features.
# Features and Capabilities
![](ui.png#noborder "ui")
***Mantra** ui during a backtest*

- Multicore
- **NO ASYNC**
- Low latency inter core communication using hand crafted message **queues, seqlocks, and shared memory**
- Full internal observability (message tracking) and high performance **in-situ telemetry** of latency and business logic
- Full **persistence of messages** using an efficiently compressed encoding for post execution analysis and replay
- [L2](https://centerpointsecurities.com/level-1-vs-level-2-market-data/) based orderbooks
- Concurrent handling of multiple trading algorithms
- Balance and order tracking accross multiple `Exchanges`
- Continuous **ingestion and storage** of market data streams
- WebSocket based connections to 5 crypto `Exchanges` (Binance, Bitstamp, Bitfinex, Coinbase and Kraken)
- "In production" **backtesting** by replaying historical marketdata streams while mimicking execution with a **mock exchange**
- Real-time UI for analysis of the system, marketdata and backtesting, capable of handling millions of datapoints
- ~500k msgs/s throughput @ 0.5 - 10 microseconds internal latency
- A focus on code simplicity and **locality of behavior**
- < 15K LOC (for now)

# System Design Overview
The core design has changed very little since **Mantra's** inception.
One of the main principles that has always guided me is a strong focus on pragmatism.
After all, creating a fully fledged and capable low-latency trading engine, while learning a new programming language, is no small project to tackle in one's free time.
A very modular and disconnected system design with isolated parts would allow me to effeciently add features while also allowing me to refactor previous implementations as
I gathered more experience working with `rust`.

This made me to opt for an event based system where `Actors` communicate with eachother by passing messages through `Queues`.
The potentially lower latency *single-function-hot-path* approach inevitably leads to more intertwined code, and I am convinced that it is easier to be build on top of a solid distributed system rather than the other way around.

The schematic below shows a high-level overview of the current design of **Mantra**
![](system_design.svg#noborder)
*Fig 1. High level design of **Mantra***

The main execution logic and data flow is quite straight-forward:
1. incoming `L2Update` and `TradeUpdate` market data messages get consumed by the `TradeModels`
2. each `TradeModel` fills out a pre-defined set of ideal order positions, each with an `InstrumentId`, `price` and `volume`
3. the `Overseer` actor continuously loops through these, and compares them to previously sent `OrderRequests` and live `Orders` on the target `Exchange`
4. if they don't match up and the necessary `balance` is available on the `Exchange`, the `Overseer` generates and publishes a new `OrderRequest`
5. the `AccountHandler` connecting with the target `Exchange` will then consume and send these requests, while feeding back various updates to the `Overseer`

An added benefit of centering the system around multi-consumer broadcasting message `Queues` is that it becomes extremely easy to attach a number of non latency critical auxiliary `Actors` that handle tasks such as **logging** without impacting the performance of the main business logic.

Lastly, as **crypto markets** are for now the main target (**Mantra** is by no means built specifically for crypto), the vast majority of latency anyway originates from the connection between me and the exchanges. I hope to be able to co-locate with certain exchanges in the future, at which point a single-function-hot-path becomes much more attractive for certain applications. Again, I believe a solid distributed system can form the perfect starting point for this as well.

That is why I really strived to keep the internal latency as low as possible at every step of the way. The result is that **Mantra** achieves internal tick-to-trade latencies between 400ns and 10$\mu$s on an untuned arch-linux based distro running on a less than prime example of the intel 14900 K.

## Inter Core Communication (ICC)

![](Queue.svg#noborder)
*Fig 2. Seqlocked Buffer*

Let's zoom in to one of the fundamental parts of the system: the inter core communication layer.

The first part consists of the message `Queues`, denoted in [Fig 1.](@/posts/hello_world/index.md#system-design-overview) by the red arrows and ovals that specify the message type of each queue.
They are essentially [`Seqlocked`](https://en.wikipedia.org/wiki/Seqlock) ringbuffers that can be used both in *single-producer-multi-consumer* (SPMC) and *multi-producer-multi-consumer* (MPMC) modes.

The main design considerations for their application in **Mantra** were:
- Achieve a close to the ideal ~30ns core-to-core latency (see e.g. [anandtech 13900k and 13600k review](https://www.anandtech.com/show/17601/intel-core-i9-13900k-and-i5-13600k-review/5) and the [fantastic core-to-core-latency tool](https://github.com/nviennot/core-to-core-latency))
- Every attached `Consumer` gets every message, also known as *broadcast* mode
- `Producers` are "not" impacted by number of attached `Consumers` (difficult to achieve perfectly), mainly they don't care if `Consumers` can keep up
- `Consumers` should not impact eachother, and should know when they got sped past by `Producers`

Considering these design goals, `Producers` and `Consumers` do not share any state but the ringbuffer itself. `Consumers` simply know which version of the `Seqlocks` guarding the data they expect.
This means they know when the next message is ready: a `Producer` has incremented the version of the next `Seqlock` to the expected one,
as well as when they got sped past: a `Producer` incremented the version of the next `Seqlock` at least twice making it too high.

In **Mantra**, aside from the `Consumers` that handle the business logic, each `Queue` also has a `Consumer` that persists each message to disk.

The second part of the ICC layer are the `SeqlockVectors` denoted by the blue rectangles in [Fig 1.](@/posts/hello_world/index.md#system-design-overview). They are used between the `TradeModels` and the `Overseer`.
These were chosen over another `Queue` because `TradeModels` potentially recalculate their ideal positions on each incoming marketdata message.
The `Overseer` takes care of quite some tasks. If it was busy while a `TradeModel` recomputed the values for a given ideal `Order` multiple times, the `Overseer` would still have to go through the messages from oldest to newest.
Using a `SeqlockVector` means that the `TradeModels` can update their desired positions as often as they want and the `Overseer` will always potentially send `OrderRequests` based on the latest information.

One big benefit of using the style of communication is that by using shared memory any process can safely observe the messages flying through `Queues` and access the data filled
in the `SeqlockVectors`. As we will see later on this is very useful for offloading ancillary tasks to external tools.

In the next blog post I will do a much deeper dive on this layer, so stay tuned for that!

## Telemetry and Observability

From the very beginning I put great emphasis on in-situ telemetry to keep the performance of different parts of **Mantra** in check at all times.

Given the design of **Mantra** I decided:
- To use the hardware timer [`rdtscp`](https://www.felixcloutier.com/x86/rdtscp) for timestamping: more accurate, less costly than OS timestamps
- That each entering message gets an **origin** timestamp that is **propagated** to all downstream messages that originate from it
- That when a message gets **published**, a timestamp is taken and its `delta` w.r.t. the **origin** timestamp is stored together with the publisher's `id`
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
This is especially true for *low-latency* systems, given how tightly coupled the performance and implementation of the system are with the trading algos and their parameters.

I have thus implemented a `MockExchange` which feeds the captured historic market data back into the system, and simultaneously uses it to mimic the behavior of a real `Exchange`.
Of course there are some approximations here, but it nonetheless provides a successful strategy for backtesting the system as a whole.

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
