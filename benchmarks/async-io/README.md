# Async I/O experiment

Run the offline comparison from the repository root:

```sh
ulimit -n 1024
cargo run --release --manifest-path benchmarks/async-io/Cargo.toml
cargo test --manifest-path benchmarks/async-io/Cargo.toml
```

The standalone crate keeps `ureq` out of the application. Its blocking client
uses the RSS configuration from the pre-migration source: 30 second timeout,
five redirects, the same user agent and 8 MiB body limit. Each blocking request
runs on its own new native thread. The async side uses reqwest and the
production bounded reader, one Tokio service executor, and a blocking pool for
JSON parsing. Both clients keep a shared connection pool across rounds.

An independent loopback server returns the same 26 KiB JSON body in four chunks,
waiting 10 ms before each. A 2 ms timer measures the service scheduler's largest
heartbeat gap. Four rounds alternate client order at 1, 8, 32 and 128 concurrent
requests. The raised descriptor limit allows both ends of 128 loopback sockets.
No public service, TLS handshake, account, credential or remote data is involved.

A local release run on 2026-09-09 on an Apple M5 Pro with 64 GiB RAM and macOS
26.6.2 produced the following medians, calculated
from the four `elapsed_ms` values in [results.csv](results.csv) for each
client and concurrency. The CSV records milliseconds to three decimal places;
averaging the middle two observations can add a fourth decimal place. Heartbeat
figures are the maximum `max_heartbeat_gap_ms` across those four rounds, not a
frame-rendering measure.

| Concurrent requests | Threads elapsed ms | Tokio elapsed ms | Threads heartbeat ms | Tokio heartbeat ms |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 49.2810 | 50.2445 | 2.527 | 2.999 |
| 8 | 51.4165 | 50.9190 | 2.538 | 2.732 |
| 32 | 51.5270 | 51.1590 | 2.538 | 3.153 |
| 128 | 55.3225 | 53.0615 | 3.023 | 3.420 |

This run shows roughly 4% lower elapsed time at 128 requests and no measured
heartbeat improvement. Async execution primarily removes a blocked service
thread per concurrent operation. It cannot reduce the server's delay. These
small timing differences should not be treated as a general performance win.

This compares transport and execution patterns, not whole application builds.
Native workers in the original app could persist across requests; this benchmark
includes thread startup each round. It does not measure SQLite contention,
TDLib behavior, TLS, actual UI frames, memory usage or CPU-intensive documents.
Those need separate application-level measurements. The test command also runs
the actual production HTTP and SSE regression tests against offline fixtures.

The existing `apps::telegram::tests::performance::startup_operation_poll_timing`
and `disk_chat_switch_timing` tests were also run unchanged against the immutable
pre-migration test binary and the migrated native test binary. Their four
alternating rounds are recorded in [telegram-results.csv](telegram-results.csv).
To run them in either checkout:

```sh
cargo test -p superapp startup_operation_poll_timing -- --ignored --nocapture
cargo test -p superapp disk_chat_switch_timing -- --ignored --nocapture
```

Both tests use fake providers and inline workers; the disk case builds a fresh
temporary store containing 10,000 messages. No native window is rendered. The
baseline binary used `--no-default-features`, the migrated binary the native
defaults; these fixture paths do not call TDLib. All samples are retained,
including the first baseline chat-open outlier of 5.13 ms. Median request-poll
time was 87.30→85.64 µs, chat-open preparation 276.17→257.02 µs, transcript
completion 32.74→31.39 ms and cached transcript access 157→169 ns. These small
changes are not an end-to-end UI performance result.
