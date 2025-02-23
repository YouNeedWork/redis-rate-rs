# redis-rate-rs

> This crate is based on a Redis script implementation of
> "The generic cell rate algorithm" (GCRA) from [rwz/redis-gcra](https://github.com/rwz/redis-gcra).
> And it's inspired by a similar Go package: [go-redis/redis_rate](https://github.com/go-redis/redis_rate).

Rate Limiter depends on Redis for your favorite distributed Rust applications.
Simply create a [redis-rs/redis-rs](https://github.com/redis-rs/redis-rs) client and enjoy!

Changes are made to make redis-rate become faster and more useful:

1. An improved version of GCRA script which can always return correct remaining quota.
2. A `local_accelerate` feature to save Redis calls when the quota is not enough.

## Basic Usage

Add this to your `Cargo.toml`:

```toml
[dependencies]
redis-rate = "0.1"
```

Then write the following code:

```rust
let redis_client = redis::Client::open("redis://127.0.0.1/")?;
let limiter = redis_rate::Limiter::new(redis_client);

let limit = redis_rate::new_limit!(1, 5, 10); // 1 request per 10 seconds, burst 5
let result = limiter.allow("my_key", limit)?;
```

In the result, you will get info including `limited`, `remaining`, `retry_after` and `reset_after`
to help you decide what to do next.

> Should be mentioned that,
> `burst` can't be smaller than `rate` in this crate,
> although it's not a strict requirement in GCRA algorithm.
> You will get panic or compile error if you set burst smaller than rate.

## Examples

There is an axum server example in the `examples` directory.
Run it with `cargo run --example axum`.

## Local Accelerate

Redis calls are fast, but not free.
If you want to save some Redis calls, you can use the `local_accelerate` feature:

```toml
[dependencies]
redis-rate = { version = "0.1", features = ["local_accelerate"] }
```

This feature stores the reset times of the limits in memory.

To make this in memory cache to be reset across the instances,
a pubsub channel needs to be created.
Spawn a thread with `limiter.start_event_sync` to listen to the event in the channel.

```rust
let limiter_clone = limiter.clone();
tokio::spawn(async move {
    while let Err(e) = limiter_clone.start_event_sync() {
        eprintln!("Error: {}", e);
    }
});
```

When `limiter.reset` is called, the reset event will be published to the channel
and the listening thread will update the in memory cache.

### Performance

The longer the `emission_interval` (`period / rate`) is,
the more performance improvement you can get.
If the `emission_interval` is more than 1 second, the `local_accelerate` feature is recommended.

The following is a local bench by
[codesenberg/bombardier](https://github.com/codesenberg/bombardier),
using the `axum` example in this crate which sets the limit to 1 requests per 10 second.
The Redis is a docker container running on the same machine.

With command `cargo run --release --example axum` we got:

```text
Bombarding http://127.0.0.1:3000/ for 10s using 125 connection(s)
...
Done!
Statistics        Avg      Stdev        Max
  Reqs/sec     15633.17   11076.54   37189.63
  Latency        8.04ms     5.45ms   145.23ms
  HTTP codes:
    1xx - 0, 2xx - 154905, 3xx - 0, 4xx - 0, 5xx - 0
    others - 0
  Throughput:     3.16MB/s
```

With command `cargo run --release --example axum --features local_accelerate` we got:

```text
Bombarding http://127.0.0.1:3000/ for 10s using 125 connection(s)
...
Done!
Statistics        Avg      Stdev        Max
  Reqs/sec    117714.36   39108.03  181256.87
  Latency        1.07ms     1.28ms   116.08ms
  HTTP codes:
    1xx - 0, 2xx - 1166828, 3xx - 0, 4xx - 0, 5xx - 0
    others - 0
  Throughput:    25.46MB/s
```

You can see a 7.5x performance improvement with the `local_accelerate` feature.

## Prerequisites

Redis in version higher than 3.2 is required since the script requires `replicate commands` feature.
Rust in version higher than 1.80 is required since the crate uses `LazyLock`.

## Contributing

Please feel free to open an issue or a pull request.

But if you are asking about the specific usage of this crate,
GitHub Discussions may be a better place.
