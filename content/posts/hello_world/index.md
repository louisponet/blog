+++
title = "Hello World, this is Mantra"
date = 2024-05-24
description = "The current status and design overview of my hft trading engine, and an outline of the planned discussion topics related to it."
[taxonomies]
tags =  ["mantra", "hft", "icc"]
[extra]
comment = true
+++

I started work on **Mantra** in an effort to learn **rust** and explore the development process of a distributed, high-frequency/low-latency trading system in the language.
It targets internal [tick-to-trade](https://beeksgroup.com/blog/tick-to-trade-is-it-the-new-must-have-metric-in-trading-performance/) latencies in the low microsecond range, depending on algo complexity.
To help me keep this target, each part of the system is automatically timed, and the flow of data is fully tracked.

**Mantra** itself will remain closed source, for obvious reasons, but I feel that some of the solutions and designs supporting it are worth sharing, hence this blog.

Naturally, many of the code snippets will be written in `rust`.
The un-enlightened should not fret, however, as the concepts that I will discuss should be quite straight-forwardly translatable into any other capable programming language.

This initial post is intended to give an overview of the features and general design of **Mantra**.
It will hopefully give enough context as to set the stage for the future more technical posts.

General intro done, let's look at the current state of affairs.
# Features and Capabilities
![](ui.png#noborder "ui")
***Mantra** ui during a backtest*

- Multicore
- **NO ASYNC**
- Low latency inter core communication using hand crafted message **queues, seqlocks, and shared memory**
- Full internal observability (message tracking) and **in-situ telemetry** of latency and business logic
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
Pragmatism has always been one of the guiding principles given the scope of creating a low-latency trading engine... while learning a new programming language.

This has made me choose a very modular and disconnected design, conceptually very similar to microservices. Such a design makes adding new features and refactoring existing code relatively frictionless due to the resulting [locality of behavior](https://dev.to/ralphcone/new-hot-trend-locality-of-behavior-1g9k). The fact that I was initially still learning `rust` made the latter all the more important.

A different popular approach in low latency trading applications is the *single-function-hot-path* using callbacks. This approach, however, inevitably leads to more intertwined and thus harder to maintain code.
Such systems still need to have some kind of distributed supporting framework, anyway, to offload some of the ancillary non-latency-critical work.

One final consideration against using a single-function-hot-path approach is that **Mantra** currently targets **crypto** markets.
While there is nothing in the implementation that is particularly specific to **crypto**, it does mean that the vast majority of latency (>10s of milliseconds) actually originates from the connection between my pc and the exchanges.
However, if **Mantra** can be co-located right next to some of the exchanges in the future, it could be beneficial to further optimize internal latencies and potentially implement the *single-function-hot-path* stuff.

Bearing all of this in mind, let's have a look at a high-level overview of the current architecture in the diagram below.
## Architecture
![system design schematic](system_design.svg#noborder)
*Fig 1. High level structure of **Mantra***

As shown, **Mantra** is composed of a set of `Actors` (rounded rectangles) communicating with each other through lock-free `Queues` (red) and `SeqlockVectors` (blue).

The main execution logic and data flow is quite straight-forward:
1. incoming `L2Update` and `TradeUpdate` market data messages (green, top-left) get consumed by the `TradeModels` (in grey)
2. each `TradeModel` fills out a pre-defined set of ideal order positions
3. the `Overseer` continuously loops through these, and compares them to previously sent `OrderRequests` and live `Orders`
4. if they don't match up and the necessary `balance` is available new `OrderRequests` are made
5. the `AccountHandler` connecting with the target `Exchange` then sends these requests
6. `Order`, `Balance` and `OrderExecution` updates are fed back `Overseer`

Centering the system around multi-consumer message `Queues` immediately fullfils the code maintainability requirement: functionality is by nature localized in the different `Actors` and adding more of them does not impact the rest.

Since it's clear that **Mantra** rides or dies depending on how well the inter-core communication layer is implemented, we continue with a closer look on this part.

## Inter Core Communication (ICC)
![](Queue.svg#noborder)
*Fig 2. Seqlocked Buffer*

### `Queue`

As mentioned before, `Queues` are denoted by the red arrows in the [design schematic](@/posts/hello_world/index.md#architecture). The ovals specify the message type of each `Queue`.
They are essentially [`Seqlocked`](https://en.wikipedia.org/wiki/Seqlock) ringbuffers that can be used both in *single-producer-multi-consumer* (SPMC) and *multi-producer-multi-consumer* (MPMC) modes.

The main requirements for the `Queues` are:
- Achieve a core-to-core latency close to the ideal ~30ns (see e.g. [anandtech 13900k and 13600k review](https://www.anandtech.com/show/17601/intel-core-i9-13900k-and-i5-13600k-review/5) and the [fantastic core-to-core-latency tool](https://github.com/nviennot/core-to-core-latency))
- Every attached `Consumer` gets every message, also known as *broadcast* mode
- `Producers` are not impacted by number of attached `Consumers` and do not care if `Consumers` can keep up
- `Consumers` should not impact each other, and should know when they got sped past by `Producers`

As a result of the latter two points, `Producers` and `Consumers` do not share any state other than the ringbuffer itself.
The resulting layout is shown in [Fig. 2](@/posts/hello_world/index.md#inter-core-communication-icc).

A `Producer` goes through the `Queue`sequentially, first incrementing the version of the current `Seqlock` once, writing the data, and finally incrementing the version again.
Each `Consumer` keeps track of the `Seqlock` version they expect.

The reading process of a `Consumer` is as follows:
- Read the version
- if odd -> writer is currently writing
- if lower than expected version -> no message to be read yet
- if expected version -> read data
- read version again
- if same as before -> data read succesfully
- if changed -> data possibly corrupted so reread
- if version higher than expected version -> `Consumer` has been sped past because a `Producer` has written to the slot twice

Another, maybe obvious, benefit of this design is that `Actors` can be turned off and on at will, depending on the desired functionality.
They can even be running in different processes since the `Queues` use shared memory behind the scenes.
This is how the **UI** and **telemetry** of **Mantra** work, for example.

Finally, to allow for post-mortem analysis and replay, each `Queue` has a dedicated `Consumer` that persists each message to long term storage.

### `SeqlockVector`
The second part of the ICC layer are the `SeqlockVectors` denoted by the blue rectangles in [Fig 1](@/posts/hello_world/index.md#architecture).

They are used between the `TradeModels` and the `Overseer` to communicate "ideal" `Order` positions based on the latest market data information.
The reason I've chosen to use these rather than another `Queue` is because `TradeModels` generally recalculate the ideal positions very frequently (i.e. on each incoming marketdata message),
The `Overseer`, instead, really only cares about the latest ones to potentially send commands to the exchange.

Structure-wise they are identical to the buffer in [Fig. 2](@/posts/hello_world/index.md#inter-core-communication-icc).
The difference is that `Producers` and `Consumers` write and read any of the `Seqlocked` slots rather than sequentially.
They should really be called `Writers` and `Readers` in this case, but whatever.

## Telemetry and Observability
Another fundamental part of **Mantra** is the in-situ telemetry and message tracking.
Having this immediate feedback has always helped me have peace of mind that new features I implement do not deteriorate the overall performance of **Mantra**.

Given the low-latency requirements, I decided:
- to use the hardware timer [`rdtscp`](https://www.felixcloutier.com/x86/rdtscp) for timestamping: more accurate and less costly than OS timestamps
- that each message entering the system gets an **origin** timestamp which is then **propagated** to all resulting downstream messages
- that when a message gets **published** to a `Queue` its time `delta` w.r.t. the **origin** timestamp is stored together with the publisher's `id`
- to **offload** these timestamps to specific timing `Queues` in shared memory so **external tools** can do the actual timing analysis

This scheme makes sure that all parts of **Mantra** are automatically timed. It also helps the debugging process since the origin and evolution of each message is tracked and stored.
An example of the internal latencies of messages at different stages in the system is shown in the image below.

![](timers.png#noborder)
*Fig 3. timekeeper tui (top) and per `OrderRequest` message internal latency (bottom)*

The reason why sell requests are in general slower in this particular example is that this `TradeModel` produces 4 order position updates for each market data update. Two buys followed by two sells.
Seeing this plot highlights that this is a point of potential improvement.

## Market Data
The final and arguably most important piece of the puzzle is market data.
Currently, **Mantra** uses [L2](https://centerpointsecurities.com/level-1-vs-level-2-market-data/) orderbook data and trade execution data.
This is mainly because most exchanges readily provide these in the public domain, whereas not all have public L3 data streams.

    L2 means many price levels, each with the total volume of open orders for that price.
    L3 provides single order granularity.

Messages are currently streamed through **WebSockets**, but I plan to implement [FIX](https://www.investopedia.com/terms/f/financial-information-exchange.asp) connections to exchanges that support it.

### L2 Orderbooks
There is one `L2OrderBook` per `Instrument` and per `Exchange`. Each of these is updated based on streamed `L2Update` messages coming from the exchanges.

    An L2Update contains a `price` and current `volume` on that pricelevel.
    If a `volume == 0.0` the pricelevel can be removed from the order book.

This makes [Binary Search Trees](https://en.wikipedia.org/wiki/Binary_search_tree) a natural choice to implement the `bid` and `ask` sides of the order book.
They have `O(log(n))` time complexity for inserting, deleting and updating price levels.
In-order scanning is also fast since they are ordered.
I have chosen to use a `BTree` in **Mantra** instead of the standard binary tree because it is balanced and more cache friendly making it overall much faster.
It also doesn't hurt that the `rust` standard library comes with an excellent out-of-the-box implementation: the [`BTreeMap`](https://doc.rust-lang.org/std/collections/struct.BTreeMap.html).

Another interesting tree for this application is the [Van Emde Boas tree](https://en.wikipedia.org/wiki/Van_Emde_Boas_tree) (`vEB`).
It is theoretically faster than a `BTree`, but the incredible sparsity of price levels in **crypto** makes it unusable.
A `vEB` tree requires a predefined universe of keys (prices in our case), and uses `O(2^M)` memory where `M` is the number of different price levels to potentially store.
While there are ways to reduce this, it remains in general prohibitive when handling many **crypto** instruments on different exchanges.
`BTreeMaps`, instead, use `O(n)` amount of memory where `n` is the number of price levels with nonzero volume.

### Continuous Data Capture
**Mantra** continuously captures and archives all incoming `L2Update` and `TradeUpdate` messages for the 5 `Exchanges` I am currently connecting to: Binance, Bitfinex, Bitstamp, Coinbase and Kraken.
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

I am of the opinion that representative **backtests** should be performed as much as possible on the **"in-production"** system rather than through some idealized transformations of DataFrames (although that is perfect for initial strategy exploration).
This is especially true for low-latency systems, given how tightly coupled the performance and implementation of the system are with the `TradeModels` and their parameters. A faster system requires the `TradeModel` to predict less far into the future.

Having access to the full historical `L2Update` data stream has allowed me to implemented a `MockExchange` for this purpose.
It feeds the messages back into the system while simultaneously using them to mimic the behavior of a real `Exchange`.
This allows it to execute our orders as if they were part of the historical market.
While it makes the backtests not fully deterministic, this method is quite successful to backtest the system as if it were live.

We will continue exploring this rather deep topic in a future series of posts.

# Planned Blog Posts
This brings us to the end of this first post, glad you made it!
I hope this overview of the current state of **Mantra** has piqued your interest for the future posts I have planned.
The exact titles and number are subject to change, the topics to be discussed, however, are not:
1. **Low latency inter core/process communication**; `Seqlocks`, `Queues` and `SeqlockVectors`
2. **in-situ telemetry and message tracking**; Details on how every message is tracked and every part of **Mantra** is timed
3. **Market Data**; `L2Orderbooks`, ingestion and storage of data streams for backtesting, the implementation of a `MockExchange`, and much more
4. **UI**; A closer look at `egui` and how it can be used as a capable timeseries analysis tool

See you there!
# Planned Future Work on **Mantra**
- [ ] FIX connections
- [ ] L3 data
- [ ] Order queue position in the `MockExchange`
- [ ] Improved parsing of market data messages and direct NIC access
- [ ] More backtest performance metrics
- [ ] More advanced market data based signal generation
