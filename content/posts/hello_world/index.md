+++
title = "Hello World, this is Mantra"
date = 2024-05-24
description = "The current status and design overview of my hft trading engine, and an outline of the planned discussion topics related to it."
[taxonomies]
tags =  ["mantra", "hft", "icc"]
[extra]
comment = true
+++

I've started working on **Mantra** mid-June 2023, in an effort to learn **rust** and explore the development process of a distributed, high(ish)-frequency/low-latency trading system in the language.
One of my targets was on the order of 0.5 - 10$\mu$s [tick-to-trade](https://beeksgroup.com/blog/tick-to-trade-is-it-the-new-must-have-metric-in-trading-performance/) internal latency (depending on trading algo complexity).
I am happy with the solutions and design I came up with, and think there is enough genuinly useful material to serve as inspiration for those seeking to embark on a similar endeavor. While I will communicate code snippets through `rust`, the concepts are what's important and everything should be straight-forwardly translatable into any other programming language.

The code will reamin closed-source for obvious reasons, but I will go through and give a deeper discussion on some of its core building blocks in this blog.
Most of these are hand-crafted and purpose-built with a strong focus on pragmatism given the scope of the project.

My intentions are of course not purely altruistic, and in the ideal case I also hope to be able to learn a thing or two through the comments of more seasoned visitors. Time will tell.

Let's first level the playing field with an overview of the current state of affairs and overall design of the system. 

# Features and Capabilities
![](ui.png#noborder "ui")
***Mantra** ui during a backtest*

- Multicore
- **NO ASYNC**
- Low latency int(er/ra) process communication using hand crafted message **queues, seqlocks, and shared memory**
- Full internal observability (message tracking) and high performance **in-situ telemetry** of latency and business logic
- Full **persistence of messages** in a highly efficient encoding for post execution analysis and replay
- [L2](https://centerpointsecurities.com/level-1-vs-level-2-market-data/) based orderbooks
- Concurrent handling of multiple algorithms
- Balance and order tracking accross multiple _Exchanges_
- Continuous **ingestion and storage** of market data streams
- WebSocket connections to 5 crypto _Exchanges_ (Binance, Bitstamp, Bitfinex, Coinbase and Kraken)
- "In production" **backtesting** by replaying the historical streams, and mimic execution with a **mock exchange**
- High-performance realtime UI for analysis of the system and marketdata, handling millions of datapoints
- ~500k msgs/s throughput @ 0.5 - 10 microseconds internal latency
- A focus on code simplicity and **locality of behavior**
- < 15K LOC (for now)

# System Design Overview
The core design has changed very little since **Mantra's** inception. It is based around message passing between a couple core systems or _Actors_.

![](system_design.svg#noborder)
*Fig 1. High level design of **Mantra***

As shown in the picture, the data flow is quite straight-forward. Incoming _L2Update_ and _TradeUpdate_ market data messages get consumed by the _TradeModels_, which each fill out a pre-defined set of ideal order positions, each with a _InstrumentId_, _price_ and _volume_.

The _Overseer_ continuously loops through these and compares them to previously sent _OrderRequests_ and _Orders_ that are currently live on the _Exchanges_.
If they don't match up and the necessary _balance_ is available on the target _Exchange_, the _Overseer_ sends an _OrderRequest_ which the correct _AccountHandler_ will then convert into the actual request sent to the exchange.
Various updates from the different _Exchanges_ are then fed back to the _Overseer_.

You might ask yourself why I chose for a distributed design, considering that I'm targeting low latency where a single hot path is the name of the game.
The answer is to some degree my interest in distributed systems, but mainly pragmatism.

With the resources at my disposal, it makes sense to separate out concerns into different subsystems which allows each of them to handle their well-defined tasks more efficiently.
One _Overseer_, _MarketDataHandler_ and _AccountHandler_ can serve the order flow for many _TradeModels_, each handling potentially many _Instruments_, etc.

Moreover, the multi-consumer broadcasting message queues that form the spine of the system naturally allow attaching non latency critical auxiliary _Actors_ that handle tasks such as **logging** without impacting the performance of the main execution path.

Lastly, as **crypto markets** are for now the main target (**Mantra** is by no means specifically built for crypto), the vast majority of latency anyway originates from the connection between me and the exchanges.

Nonetheless, I really strived to keep the internal latency given the design constraints to the absolute minimum, and **Mantra** achieves internal latencies between 400ns and 10$\mu$s on an untuned arch-linux based distro running on a less than prime example of the intel 14900 K.

## Inter Core Communication (ICC)

![](Queue.svg#noborder)
*Fig 2. Seqlocked Buffer*

Let's zoom in to one of the fundamental parts of the system: the inter core communication layer.

The first part consists of the message _Queues_, denoted in [Fig 1.](@/posts/hello_world/index.md#system-design-overview) by the red arrows and ovals that specify the message type of each queue.
They are essentially [_Seqlocked_](https://en.wikipedia.org/wiki/Seqlock) ringbuffers that can be used both in *single-producer-multi-consumer* (SPMC) and *multi-producer-multi-consumer* (MPMC) modes.

The main design considerations for their application in **Mantra** were:
- Achieve a close to the ideal ~30ns core-to-core latency (see e.g. [anandtech 13900k and 13600k review](https://www.anandtech.com/show/17601/intel-core-i9-13900k-and-i5-13600k-review/5) and the [fantastic core-to-core-latency tool](https://github.com/nviennot/core-to-core-latency))
- Every attached _Consumer_ gets every message, also known as *broadcast* mode
- _Producers_ are "not" impacted by number of attached _Consumers_ (difficult to achieve perfectly), mainly they don't care if _Consumers_ can keep up
- _Consumers_ should not impact eachother, and should know when they got sped past by _Producers_

Considering these design goals, _Producers_ and _Consumers_ do not share any state but the ringbuffer itself. _Consumers_ simply know which version of the _Seqlocks_ guarding the data they expect.
This means they know when the next message is ready: a _Producer_ has incremented the version of the next _Seqlock_ to the expected one,
as well as when they got sped past: a _Producer_ incremented the version of the next _Seqlock_ at least twice making it too high.

In **Mantra**, aside from the _Consumers_ that handle the business logic, each _Queue_ also has a _Consumer_ that persists each message to disk.

The second part of the ICC layer are the _SeqlockVectors_ denoted by the blue rectangles in [Fig 1.](@/posts/hello_world/index.md#system-design-overview). They are used between the _TradeModels_ and the _Overseer_.
These were chosen over another _Queue_ because _TradeModels_ potentially recalculate their ideal positions on each incoming marketdata message.
The _Overseer_ takes care of quite some tasks. If it was busy while a _TradeModel_ recomputed the values for a given ideal _Order_ multiple times, the _Overseer_ would still have to go through the messages from oldest to newest.
Using a _SeqlockVector_ means that the _TradeModels_ can update their desired positions as often as they want and the _Overseer_ will always potentially send _OrderRequests_ based on the latest information.

One big benefit of using the style of communication is that by using shared memory any process can safely observe the messages flying through _Queues_ and access the data filled
in the _SeqlockVectors_. As we will see later on this is very useful for offloading ancillary tasks to external tools.

In the next blog post I will do a much deeper dive on this layer, so stay tuned for that!

## Telemetry and Observability

From the very beginning I put great emphasis on in-situ telemetry to keep the performance of different parts of **Mantra** in check at all times.

Given the design of **Mantra** I decided:
- To use the hardware timer [_rdtscp_](https://www.felixcloutier.com/x86/rdtscp) for timestamping: more accurate, less costly than OS timestamps
- That each entering message gets an **origin** timestamp that is **propagated** to all downstream messages that originate from it
- That when a message gets **published**, a timestamp is taken and its _delta_ w.r.t. the **origin** timestamp is stored together with the publisher's _id_ 
- To offload these timestamps to specific timing _Queues_ in **shared memory** so external tools can do the timing analysis

This scheme allows me to automatically time the different parts of **Mantra** with minimal overhead and to a high degree of accuracy.
It also allows to track the origin and "ancestry" of all messages in the system which is invaluable while debugging or optimizing.

Together with the small _timekeeper_ tui tool and the fully fledged _ui_ shown below, this methodical focus on observability has allowed me to make informed implementation decisions every step of the way.

![](timers.png#noborder)
*Fig 3. timekeeper tui and per _OrderRequest_ message internal latency*

## Market Data
The final and arguably most important piece of the puzzle is market data.
I chose to focus on [L2](https://centerpointsecurities.com/level-1-vs-level-2-market-data/) orderbook data and trade execution data, since most exchanges readily provide those in the public domain.
For now this is streamed through **WebSockets**, although I plan to implement [FIX](https://www.investopedia.com/terms/f/financial-information-exchange.asp) exchanges that support it.

**Mantra** handles _L2OrderBooks_ per _Instrument_ per _Exchange_. They are implemented in a relatively standard way, using _BTreeMaps_ for the _bid_ and _ask_ sides to facilitate easy insertion, removal and in-order scanning of price levels.

What is perhaps more interesting is that **Mantra** is continuously capturing and archiving all incoming _L2Update_ and _TradeUpdate_ messages for the 5 _Exchanges_ I am currently connecting to: Binance, Bitfinex, Bitstamp, Coinbase and Kraken.
With "all" I mean literally all, i.e. the data for every single spot _Instrument_ that is tradable on these exchanges.
This leads to quite a lot of data, so I use [_bitcode_](https://docs.rs/bitcode/latest/bitcode/#) together with [_zstd_](https://docs.rs/zstd/latest/zstd/#) to achieve quite impressive data compression ratios of ~20x.

Some quick numbers are:
- 169 simultaneous websocket connections
- 5764 _Instruments_ accross 5 exchanges
- up to 200k msgs/s handled
- up to 47 Gb / day
- 2.9 Tb worth of data so far
- ~40% of 1 core used for processing at rates of 35k msgs/s
- **STILL NO ASYNC**

"But why gobble up all that data", you may wonder. Glad you asked.

I am of the very strong opinion that representative **backtests** should be performed as much as possible on the **"in-production"** system rather than through some idealized transformations of DataFrames (although that is perfect for initial strategy exploration).
This is especially true for *low-latency* systems, given how tightly coupled the performance and implementation of the system are with the trading algos and their parameters.

I have thus implemented a _MockExchange_ which feeds the captured historic market data back into the system, and simultaneously uses it to mimic the behavior of a real _Exchange_.
Of course there are some approximations here, but it nonetheless provides a successful strategy for backtesting the system as a whole.

This gives only a very short glimpse into this relatively deep topic, and I have some interesting additional experiments planned for the multi series of blog posts on Market Data.

# Planned Blog Posts
Based on the overview the initially planned blog posts are:
1. **Low latency inter core/process communication**; Seqlocks, broadcasting message queues and synchronized arrays
2. **High performance in-situ telemetry and message tracking**; How every message is tracked and every part of **Mantra** is timed
3. **Market Data**; Large scale ingestion and storage, _MockExchange_ and L2 Orderbooks
4. **UI**; A closer look at _egui_ and how it can be used as a high performance timeseries analysis tool

# Planned Future Work on **Mantra**
- [ ] FIX connections
- [ ] Order queue position in the _MockExchange_
- [ ] Improved parsing of market data messages and direct NIC access
- [ ] More backtest performance metrics
- [ ] More advanced market data based signal generation
