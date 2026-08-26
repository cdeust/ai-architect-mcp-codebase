// lsp_client::frames — the bounded channel between the blocking reader and the
// caller's deadline.
//
// Split from `lsp_client::protocol` when that file crossed the §4.1 cap, along
// the seam between "frame these bytes" and "deliver frames to a caller that can
// give up". The framing below is pure over a byte stream; this half owns a
// thread, a queue and a timeout.

use super::protocol::{read_lsp_message, FrameError, LSP_TIMEOUT_PREFIX};
use serde_json::Value;
use std::io::BufReader;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

/// How many parsed frames may sit unread before the reader thread blocks.
///
/// The bound is the point of it. An UNBOUNDED channel replaces the OS pipe's
/// backpressure with none at all: a notification-heavy server — rust-analyzer
/// emits `$/progress` continuously while it indexes — would have its frames
/// eagerly read AND JSON-parsed into a queue nobody drains between requests,
/// so memory grows with the server's chatter rather than with our demand. With
/// a bound, a full queue simply stops the reader in `read_line`, which is
/// exactly where the kernel used to stop it.
///
/// Timeout semantics are untouched: the deadline lives on the receiving end,
/// and a blocked sender cannot extend it.
///
/// source: provisional heuristic. It must exceed the notification burst a
/// server emits between two of our requests (a handful for the servers this
/// drives) and stay small enough that the queue is not itself the leak. 64 sits
/// well above the former and far below the latter; calibrate against a measured
/// server that stalls on it.
const FRAME_QUEUE_DEPTH: usize = 64;

/// Moves `reader` onto a thread that pushes every frame down a bounded channel,
/// and returns the receiving end.
///
/// This is what makes the caller's timeout real. The blocking framing reads
/// live on a thread whose fate does not matter — when the child is killed on
/// `LspClient::drop`, its stdout closes, `read_lsp_message` returns the EOF
/// error, the loop ends and the thread exits. The caller waits on the channel
/// with a deadline it can actually enforce.
pub(super) fn spawn_frame_reader(
    stdout: std::process::ChildStdout,
) -> Receiver<Result<Value, FrameError>> {
    let (tx, rx) = mpsc::sync_channel(FRAME_QUEUE_DEPTH);
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            // Only a FRAMING or IO failure ends the run. A malformed payload
            // leaves the stream byte-aligned, and one bad notification must not
            // kill LSP resolution for every request that follows it — which is
            // what exiting on any error did, mislabelling every later request
            // `eof_before_header`.
            let frame = read_lsp_message(&mut reader);
            let stop = matches!(frame, Err(FrameError::Fatal(_)));
            let sent = tx.send(frame);
            // A closed receiver means the client was dropped; stop.
            if sent.is_err() || stop {
                return;
            }
        }
    });
    rx
}

/// Waits up to `timeout` for the next frame.
///
/// A `RecvTimeoutError::Timeout` becomes the module's timeout sentinel, so it
/// classifies identically to one raised anywhere else here. `Disconnected`
/// means the reader thread ended — it has already sent whatever error ended it,
/// so anything after that is EOF.
pub(super) fn next_frame(
    frames: &Receiver<Result<Value, FrameError>>,
    timeout: Duration,
) -> Result<Value, FrameError> {
    match frames.recv_timeout(timeout) {
        Ok(frame) => frame,
        Err(RecvTimeoutError::Timeout) => Err(FrameError::Timeout(format!(
            "{LSP_TIMEOUT_PREFIX} no frame within {timeout:?}"
        ))),
        Err(RecvTimeoutError::Disconnected) => Err(FrameError::Fatal(
            "eof_before_header: child stdout closed without LSP framing".to_string(),
        )),
    }
}

/// Drains every frame already queued, without waiting.
///
/// Called before writing a request. The reader thread blocks when the bounded
/// queue is full; a blocked reader stops draining the server's stdout, the
/// server's stdout pipe fills, the server stops reading OUR stdin, and our next
/// blocking write deadlocks against it. Emptying the queue first breaks that
/// cycle at the only point in it we control.
///
/// Discarding is safe here: every request is followed immediately by a read for
/// its own id, so anything still queued when the NEXT request goes out is
/// server-initiated traffic that `read_response_for_id` would have skipped
/// anyway.
pub(super) fn drain_pending(frames: &Receiver<Result<Value, FrameError>>) {
    while frames.try_recv().is_ok() {}
}

#[cfg(test)]
mod frame_bound_tests {
    // Reachable only from these tests, so gated with them: `clippy
    // --all-targets` compiles the lib separately from unittests.
    use super::*;
    use std::sync::mpsc;

    /// B.6. The bound has to hold when the server sends a partial header and
    /// then simply stops — the shape a hung language server presents.
    ///
    /// Before this change the deadline was an `Instant` compared at the top of
    /// the header loop, and `read_line` blocks in the kernel, so control never
    /// returned to the check: `read_lsp_message` sat there for as long as the
    /// child lived and took the indexer with it. A timeout consulted only
    /// between blocking calls is not a timeout.
    ///
    /// The assertion is on the RETURNED VALUE, never on elapsed time: this must
    /// not become a test whose verdict depends on machine load. A channel that
    /// never receives is what the pre-change code produced, and this test would
    /// hang there rather than fail — which is why the fix moves the blocking
    /// read off the caller's thread instead of tightening the check.
    #[test]
    fn next_frame_times_out_when_no_frame_ever_arrives() {
        // A sender held open with nothing sent models a server that wrote a
        // partial header and stopped: the reader thread is still blocked, so
        // no frame is ever pushed.
        let (_tx, rx) = mpsc::channel::<Result<Value, FrameError>>();
        let err = next_frame(&rx, Duration::from_millis(50))
            .expect_err("a frame that never arrives must not block forever");
        assert!(
            matches!(err, FrameError::Timeout(_)),
            "must classify as this module's timeout"
        );
    }

    /// A reader thread that has ended (child gone) reports EOF, not a timeout —
    /// the two are different answers and the probe classifier maps them to
    /// different reason codes.
    #[test]
    fn next_frame_reports_eof_when_the_reader_thread_is_gone() {
        let (tx, rx) = mpsc::channel::<Result<Value, FrameError>>();
        drop(tx);
        let err = next_frame(&rx, Duration::from_millis(50)).expect_err("no frame");
        assert!(
            !matches!(err, FrameError::Timeout(_)),
            "a dead reader is not a timeout"
        );
        assert!(err.message().contains("eof_before_header"));
    }

    /// The queue is BOUNDED, so a chatty server cannot grow it without limit —
    /// a full queue stops the reader thread in its blocking read, which is
    /// where the OS pipe used to stop it before the thread existed. Filling it
    /// must not affect what a waiting caller sees.
    #[test]
    fn a_full_queue_blocks_the_producer_without_disturbing_the_consumer() {
        let (tx, rx) = mpsc::sync_channel::<Result<Value, FrameError>>(FRAME_QUEUE_DEPTH);
        for i in 0..FRAME_QUEUE_DEPTH {
            tx.try_send(Ok(serde_json::json!({ "id": i })))
                .expect("the queue accepts up to its depth");
        }
        assert!(
            tx.try_send(Ok(serde_json::json!({"id": "overflow"})))
                .is_err(),
            "past its depth the queue must refuse, which is what blocks a \
             real sender instead of growing memory"
        );
        // The consumer still reads in order, and draining makes room again.
        let first = next_frame(&rx, Duration::from_millis(50)).expect("queued frame");
        assert_eq!(first.get("id").and_then(|v| v.as_i64()), Some(0));
        tx.try_send(Ok(serde_json::json!({"id": "now fits"})))
            .expect("a drained slot is reusable");
    }

    /// Frames the thread already pushed are delivered without waiting.
    #[test]
    fn next_frame_delivers_a_queued_frame() {
        let (tx, rx) = mpsc::channel::<Result<Value, FrameError>>();
        tx.send(Ok(serde_json::json!({"id": 1}))).expect("send");
        let msg = next_frame(&rx, Duration::from_millis(50)).expect("queued frame");
        assert_eq!(msg.get("id").and_then(|v| v.as_i64()), Some(1));
    }

    /// Re-review finding 3. The reader thread exited on ANY error, including a
    /// JSON-parse failure on a body that had been read IN FULL — the stream is
    /// still byte-aligned there, and the next frame is perfectly readable. One
    /// malformed notification therefore killed LSP resolution for the rest of
    /// the run, with every later request mislabelled `eof_before_header`.
    ///
    /// The classification is what the thread's loop keys on, so it is the
    /// classification this pins.
    #[test]
    fn a_malformed_payload_is_recoverable_but_broken_framing_is_not() {
        let payload = read_frame_error(b"Content-Length: 3\r\n\r\nnot");
        assert!(
            matches!(payload, Some(FrameError::Payload(_))),
            "a fully-consumed body that will not parse costs one frame only"
        );

        let no_header = read_frame_error(b"garbage without framing\r\n\r\n");
        assert!(
            matches!(no_header, Some(FrameError::Fatal(_))),
            "missing Content-Length leaves the stream position unknown"
        );

        let truncated = read_frame_error(b"Content-Length: 99\r\n\r\nshort");
        assert!(
            matches!(truncated, Some(FrameError::Fatal(_))),
            "a body shorter than its declared length desynchronises the stream"
        );
    }

    /// Drives `read_lsp_message`'s framing over a byte slice by way of a real
    /// child process, since its parameter is a `ChildStdout` reader.
    fn read_frame_error(bytes: &[u8]) -> Option<FrameError> {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let mut child = Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn cat");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(bytes)
            .expect("write");
        let stdout = child.stdout.take().expect("stdout");
        let mut reader = BufReader::new(stdout);
        let out = read_lsp_message(&mut reader).err();
        let _ = child.wait();
        out
    }

    /// Round-3 finding 1. The reader thread survived a Payload error, but the
    /// CONSUMER still propagated it, so one malformed notification failed the
    /// in-flight call even though the very next frame carried its answer. The
    /// previous test checked the classification only; this checks the recovery.
    ///
    /// `is_skippable` is the predicate both consumer loops key on, so it is
    /// what this pins.
    #[test]
    fn a_payload_error_is_skippable_and_the_next_frame_still_arrives() {
        let (tx, rx) = mpsc::sync_channel::<Result<Value, FrameError>>(FRAME_QUEUE_DEPTH);
        tx.send(Err(FrameError::Payload("parse JSON body: x".into())))
            .expect("queue the bad frame");
        tx.send(Ok(serde_json::json!({"id": 7})))
            .expect("queue the good one behind it");

        let first = next_frame(&rx, Duration::from_millis(50)).expect_err("the bad frame");
        assert!(
            first.is_skippable(),
            "a fully-consumed body that would not parse must be skippable"
        );
        // …and the answer behind it is still there.
        let second = next_frame(&rx, Duration::from_millis(50)).expect("the good frame");
        assert_eq!(second.get("id").and_then(Value::as_i64), Some(7));
    }

    /// The other two error kinds end the call rather than being skipped.
    #[test]
    fn framing_and_timeout_errors_are_not_skippable() {
        assert!(!FrameError::Fatal("eof_before_header: x".into()).is_skippable());
        assert!(!FrameError::Timeout("lsp_timeout: x".into()).is_skippable());
    }

    /// Finding 4. `drain_pending` empties the queue without waiting, which is
    /// what unblocks the reader thread before a blocking write.
    #[test]
    fn drain_pending_empties_the_queue_without_waiting() {
        let (tx, rx) = mpsc::sync_channel::<Result<Value, FrameError>>(FRAME_QUEUE_DEPTH);
        for i in 0..FRAME_QUEUE_DEPTH {
            tx.try_send(Ok(serde_json::json!({ "id": i })))
                .expect("fill it");
        }
        assert!(tx.try_send(Ok(serde_json::json!({"id": "over"}))).is_err());

        drain_pending(&rx);

        tx.try_send(Ok(serde_json::json!({"id": "now fits"})))
            .expect("a drained queue accepts again, which is what frees the reader");
    }
}
