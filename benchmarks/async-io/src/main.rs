//! Offline experiment: the old RSS ureq configuration on one thread per
//! request vs the new reqwest configuration on one Tokio service executor.
//! The independent local server sends equal delayed, chunked JSON bodies.
//! A 2 ms heartbeat measures scheduler stalls, including worker startup.
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[allow(dead_code)]
#[path = "../../../kernel/src/http.rs"]
mod http;
#[allow(dead_code)]
#[path = "../../../kernel/src/sse.rs"]
mod sse;

const BODY_CAP: usize = 8 << 20;
const PAUSE: Duration = Duration::from_millis(10);
const CHUNKS: usize = 4;

fn server() -> (String, tokio::sync::oneshot::Sender<()>) {
    let (address_tx, address_rx) = std::sync::mpsc::channel();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            address_tx.send(format!("http://{}/feed", listener.local_addr().unwrap())).unwrap();
            tokio::pin!(stopped);
            loop {
                tokio::select! {
                    _ = &mut stopped => break,
                    accepted = listener.accept() => {
                        let (mut socket, _) = accepted.unwrap();
                        tokio::spawn(async move {
                            let body = serde_json::json!({"items": vec!["an article"; 2048]}).to_string();
                            let mut request = Vec::new();
                            loop {
                                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                                    let mut bytes = [0; 4096];
                                    match socket.read(&mut bytes).await {
                                        Ok(0) | Err(_) => return,
                                        Ok(n) => request.extend_from_slice(&bytes[..n]),
                                    }
                                }
                                request.clear();
                                if socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n").await.is_err() { return; }
                                for chunk in body.as_bytes().chunks(body.len().div_ceil(CHUNKS)) {
                                    tokio::time::sleep(PAUSE).await;
                                    let header = format!("{:x}\r\n", chunk.len());
                                    if socket.write_all(header.as_bytes()).await.is_err()
                                        || socket.write_all(chunk).await.is_err()
                                        || socket.write_all(b"\r\n").await.is_err() { return; }
                                }
                                if socket.write_all(b"0\r\n\r\n").await.is_err() { return; }
                            }
                        });
                    }
                }
            }
        });
    });
    (address_rx.recv().unwrap(), stop)
}

fn millis(time: Duration) -> f64 {
    time.as_secs_f64() * 1000.0
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let (url, stop) = server();
    let blocking: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .max_redirects(5)
        .user_agent("superapp-rss/0.1")
        .build()
        .into();
    let asynchronous = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(5))
        .retry(reqwest::retry::never())
        .user_agent("superapp-rss/0.1")
        .build()
        .unwrap();
    println!("mode,concurrency,round,elapsed_ms,p50_ms,p95_ms,max_heartbeat_gap_ms");
    for concurrency in [1, 8, 32, 128] {
        // Alternate order each round to reduce warmup/order bias.
        for round in 0..4 {
            for asynchronous_mode in if round % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                let (heart_stop, mut stopped) = tokio::sync::oneshot::channel();
                let heartbeat = tokio::spawn(async move {
                    let mut previous = Instant::now();
                    let mut worst = Duration::ZERO;
                    let mut interval = tokio::time::interval(Duration::from_millis(2));
                    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    loop {
                        tokio::select! {
                            _ = &mut stopped => return worst,
                            _ = interval.tick() => {
                                let now = Instant::now();
                                worst = worst.max(now - previous);
                                previous = now;
                            }
                        }
                    }
                });
                tokio::task::yield_now().await;
                let start = Instant::now();
                let mut latencies = Vec::new();
                if asynchronous_mode {
                    let requests = (0..concurrency).map(|_| {
                        let client = asynchronous.clone();
                        let url = url.clone();
                        async move {
                            let start = Instant::now();
                            let response = client.get(url).send().await.unwrap();
                            let bytes = http::bounded_bytes(response, BODY_CAP).await.unwrap();
                            tokio::task::spawn_blocking(move || {
                                let value: serde_json::Value =
                                    serde_json::from_slice(&bytes).unwrap();
                                assert_eq!(value["items"].as_array().unwrap().len(), 2048);
                            })
                            .await
                            .unwrap();
                            start.elapsed()
                        }
                    });
                    latencies = futures_util::future::join_all(requests).await;
                } else {
                    let (finished, mut completions) = tokio::sync::mpsc::unbounded_channel();
                    for _ in 0..concurrency {
                        let client = blocking.clone();
                        let url = url.clone();
                        let finished = finished.clone();
                        std::thread::spawn(move || {
                            let start = Instant::now();
                            let mut response = client.get(url).call().unwrap();
                            let bytes = response
                                .body_mut()
                                .with_config()
                                .limit(BODY_CAP as u64)
                                .read_to_vec()
                                .unwrap();
                            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                            assert_eq!(value["items"].as_array().unwrap().len(), 2048);
                            finished.send(start.elapsed()).unwrap();
                        });
                    }
                    drop(finished);
                    while let Some(latency) = completions.recv().await {
                        latencies.push(latency);
                    }
                }
                let elapsed = start.elapsed();
                let _ = heart_stop.send(());
                let worst = heartbeat.await.unwrap();
                latencies.sort_unstable();
                println!(
                    "{},{concurrency},{round},{:.3},{:.3},{:.3},{:.3}",
                    if asynchronous_mode {
                        "tokio"
                    } else {
                        "threads"
                    },
                    millis(elapsed),
                    millis(latencies[latencies.len() / 2]),
                    millis(latencies[(latencies.len() * 95 / 100).min(latencies.len() - 1)]),
                    millis(worst)
                );
            }
        }
    }
    let _ = stop.send(());
}
