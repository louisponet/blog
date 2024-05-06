+++
title = "Hello World, this is Mantra"
date = 2024-05-05
description = "The current status and design overview of my hft trading engine, and an outline of the planned discussion topics related to it."
[taxonomies]
tags =  ["mantra", "hft"]
+++

I've started working on **Mantra** mid-June 2023, in an effort to learn **rust** as well as to gain a deeper understanding of what goes into developing a distributed, high(ish) frequency trading system. One of my targets was on the order of 0.5 - 10 microseconds internal latency (depending on trading algo complexity).
I am quite happy with the solutions and design I came up with, and think there is enough material that could be of use for other people seeking to embark on a similar endeavor.

For obvious reasons the code will not be made open source, but I will go through and give a deeper discussion on some of the building blocks in this blog. Most of these are hand-crafted and purpose-built for the task at hand, with a strong focus on pragmatism given the scope of the project.

My intentions are of course not purely altruistic, and in the ideal case I also hope to be able to learn a thing or two through the comments of more seasoned visitors. Time will tell.

Let's first level the playing field with an overview of the current state of affairs and overall design of the system.

# Features and Capabilities
![](ui.png#noborder "ui")
***Mantra** ui during a backtest*

- Multicore
- **NO ASYNC**
- Low latency int(er/ra) process communication using hand crafted message **queues, seqlocks and shared memory**
- Full internal observability (message tracking) and high performance **in-situ telemetry** of latency and business logic
- Full **persistence of messages** in a highly efficient encoding for post execution analysis and replay
- L2 based orderbooks
- Concurrent handling of multiple algorithms
- Balance and order tracking accross multiple execution venues
- Continuous **ingestion and storage** of market data streams
- WebSocket connections to 5 crypto `Exchanges` (Binance, Bitstamp, Bitfinex, Coinbase and Kraken)
- "In production" **backtesting** by replaying these streams, and mimic execution with a **mock exchange**
- High-performance realtime UI for analysis of the system and marketdata, handling millions of datapoints
- ~500k msgs/s throughput @ 0.5 - 10 microseconds internal latency
- A focus on simplicity and **locality of behavior**
- < 15K LOC (for now)

# System Design Overview
The core design has changed very little since **Mantra's** inception. It is based around message passing between a couple core systems or `Actors`.

![](system_design.svg#noborder)
*High level design of **Mantra***

As shown by the picture, the data flow is relatively straight-forward. Incoming `L2Update` and `TradeUpdate` market data messages get ingested by the `TradeModels`, which each fill out a pre-defined set of ideal order positions (`InstrumentId`, `price` and `volume`).

The `Overseer` continuously loops through these and compares them to previously sent `OrderRequests` and currently open `Orders`. If they don't match up and the necessary `balance` is available on the target `Exchange`, the `Overseer` sends an `OrderRequest` which the correct `AccountHandler` will then convert into an actual request sent to the exchange.

Various updates from the different `Exchanges` are then fed back to the `Overseer`.

You might ask yourself why I chose for a distributed design if I'm targeting low latency where a single hot path is the name of the game.
The answer is to some degree my interest in distributed systems, but mainly pragmatism.
I only have so many resources at my disposal, which makes separating out concerns into different subsystems the obvious choice allowing each of them to handle their well-defined tasks more efficiently.
This means I can have a single `Overseer` serve the order flow for many `TradeModels`, each handling potentially many `Instruments`. The same goes for interfaces to the `Exchanges`.
The multi-consumer message queues that from the spine of the system allow to attach auxiliary `Actors` that perform many non latency critical supporting tasks such as **logging** without impacting the performance of the main execution path.
Lastly, as **crypto markets** are currently the main target, again unfortunately for pragmatic reasons, the vast majority of latency anyway originates from the connection between me and the exchanges.

Nonetheless, I really strived to keep the latency given the design constrains to the absolute minimum, and **Mantra** achieves internal latencies between 400ns and 10 microseconds on an untuned arch-linux based distro running on a less than prime example of the intel 14900 K.

## Inter Process Communication (IPC)

# Planned Blog Posts
# Planned Future Work
