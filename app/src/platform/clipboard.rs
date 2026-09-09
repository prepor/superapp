//! Clipboard copies are accepted in order and report the actual native result.

use kernel::caps::ClipboardCopy;
use tokio::sync::{mpsc, oneshot};

struct Request {
    text: String,
    complete: oneshot::Sender<Result<(), String>>,
}

struct Queue(mpsc::UnboundedSender<Request>);

impl Queue {
    fn new(mut perform: impl FnMut(String) -> ClipboardCopy + Send + 'static) -> Self {
        let (send, mut receive) = mpsc::unbounded_channel::<Request>();
        kernel::runtime::spawn(async move {
            while let Some(request) = receive.recv().await {
                let result = perform(request.text).await;
                let _ = request.complete.send(result);
            }
        });
        Self(send)
    }

    fn copy(&self, text: String) -> ClipboardCopy {
        let (complete, result) = oneshot::channel();
        let sent = self.0.send(Request { text, complete });
        Box::pin(async move {
            sent.map_err(|_| "clipboard service stopped".to_string())?;
            result
                .await
                .map_err(|_| "clipboard service stopped".to_string())?
        })
    }
}

pub fn copy(text: String) -> ClipboardCopy {
    static QUEUE: std::sync::OnceLock<Queue> = std::sync::OnceLock::new();
    QUEUE.get_or_init(|| Queue::new(native)).copy(text)
}

fn native(text: String) -> ClipboardCopy {
    Box::pin(async move {
        #[cfg(target_os = "macos")]
        {
            use std::process::Stdio;
            use tokio::io::AsyncWriteExt;
            tokio::time::timeout(std::time::Duration::from_secs(30), async move {
                let mut child = tokio::process::Command::new("/usr/bin/pbcopy")
                    .stdin(Stdio::piped())
                    .stderr(Stdio::piped())
                    .kill_on_drop(true)
                    .spawn()
                    .map_err(|error| format!("pbcopy: {error}"))?;
                let mut stdin = child.stdin.take().ok_or("pbcopy did not open its input")?;
                stdin
                    .write_all(text.as_bytes())
                    .await
                    .map_err(|error| format!("pbcopy: {error}"))?;
                stdin
                    .shutdown()
                    .await
                    .map_err(|error| format!("pbcopy: {error}"))?;
                drop(stdin);
                let result = child
                    .wait_with_output()
                    .await
                    .map_err(|error| format!("pbcopy: {error}"))?;
                if result.status.success() {
                    Ok(())
                } else {
                    Err(format!(
                        "pbcopy exited {}: {}",
                        result.status,
                        String::from_utf8_lossy(&result.stderr).trim()
                    ))
                }
            })
            .await
            .map_err(|_| "clipboard copy timed out".to_string())?
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = text;
            Err("no clipboard on this platform".into())
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn accepted_copies_finish_in_order_and_report_failure_without_touching_native_clipboard() {
        let (release, hold) = oneshot::channel();
        let mut hold = Some(hold);
        let (started, first_started) = oneshot::channel();
        let mut started = Some(started);
        let order = Arc::new(Mutex::new(Vec::new()));
        let observed = order.clone();
        let queue = Queue::new(move |text| {
            let gate = hold.take();
            let started = started.take();
            let order = order.clone();
            Box::pin(async move {
                order.lock().unwrap().push(text.clone());
                if let Some(started) = started {
                    let _ = started.send(());
                }
                if let Some(gate) = gate {
                    gate.await.unwrap();
                }
                if text == "refused" {
                    Err("test clipboard refused".into())
                } else {
                    Ok(())
                }
            })
        });
        let first = queue.copy("first".into());
        let refused = queue.copy("refused".into());
        let last = queue.copy("last".into());
        kernel::runtime::block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(5), first_started)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(*observed.lock().unwrap(), ["first"]);
            release.send(()).unwrap();
            first.await.unwrap();
            assert_eq!(refused.await.unwrap_err(), "test clipboard refused");
            last.await.unwrap();
        });
        assert_eq!(*observed.lock().unwrap(), ["first", "refused", "last"]);
    }
}
