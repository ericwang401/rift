//! Recording and replaying everything rift learns from the system.
//!
//! The reactor's input is not only its event stream: while handling an event
//! it asks the window server questions — which space is this window on, what
//! is its live frame, what is under the pointer — and reads the clock. A
//! replay that only re-feeds events diverges from what the machine actually
//! did the moment one of those answers differs. This module records those
//! answers inline with the events and hands them back, in order, on replay,
//! so a recorded session reproduces bit for bit: Premiere's lagging frame
//! reports, macOS moving a dragged window between displays, all of it.
//!
//! Only the reactor thread's questions are recorded and replayed; other
//! threads (app actors, the event tap) reach the reactor through events,
//! which are recorded by `reactor::Record`.
//!
//! Every line in a trace after the two header lines is `Ev <ms> <event>` or
//! `Sys <line>`, both JSON (RON cannot round-trip untagged and flattened
//! serde types, which the event types use).

use std::cell::Cell;
use std::collections::VecDeque;
use std::fs::File;
use std::io::Write;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// A recorded answer to a question the reactor asked the system.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SysLine {
    pub ms: u64,
    pub kind: String,
    /// RON of the question's arguments.
    pub key: String,
    /// RON of the answer.
    pub answer: String,
}

enum Mode {
    Off,
    Record {
        file: File,
        started: Instant,
    },
    Replay {
        /// Only this thread is replaying; any other thread asking questions
        /// (another test running alongside) gets the live answer.
        thread: std::thread::ThreadId,
        /// Recorded answers still to be given, per (kind, key), in recorded
        /// order. Keyed so a question costs a lookup, not a scan of the
        /// whole recording — a long trace asks tens of thousands of times.
        answers: std::collections::HashMap<(String, String), VecDeque<SysLine>>,
        /// The latest answer given for each (kind, key): the system's state
        /// as of the current replay time, for questions asked more often on
        /// replay than they were live.
        latest: std::collections::HashMap<(String, String), String>,
        base: Instant,
        now_ms: u64,
        /// Questions the recording did not have an answer for, in order.
        misses: Vec<String>,
        /// Questions answered by the next recorded answer of the same kind
        /// because no exact key matched (floating-point drift in a cursor
        /// position, typically).
        drifts: Vec<String>,
    },
}

static MODE: Mutex<Mode> = Mutex::new(Mode::Off);
/// Fast-path flag: instrumentation is live, either into a file recording or
/// the always-on flight-recorder ring. True from startup — the ring runs
/// for the whole session so a bug can be reproduced FIRST and dumped AFTER
/// (`rift-cli execute trace dump`), instead of hoping it happens again with
/// a recording running.
static RECORDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// The flight recorder: the most recent trace lines, capped by bytes. Lines
/// carry the same format as a file recording, with `ms` relative to process
/// start.
static RING: Mutex<RingBuf> = Mutex::new(RingBuf { lines: std::collections::VecDeque::new(), bytes: 0 });
const RING_CAP_BYTES: usize = 32 * 1024 * 1024;

struct RingBuf {
    lines: std::collections::VecDeque<String>,
    bytes: usize,
}

static PROCESS_START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

fn process_elapsed_ms() -> u64 {
    PROCESS_START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// Writes the ring's contents to `path`, newest history last. The dump is
/// for reading, not replay: it has no state header, and its first line says
/// so.
pub fn dump_ring(path: &std::path::Path) -> std::io::Result<usize> {
    let ring = RING.lock().unwrap_or_else(|e| e.into_inner());
    let mut file = File::create(path)?;
    writeln!(
        file,
        "Flight {{\"dumped_at_ms\":{},\"lines\":{}}}",
        process_elapsed_ms(),
        ring.lines.len()
    )?;
    for line in &ring.lines {
        writeln!(file, "{line}")?;
    }
    Ok(ring.lines.len())
}

thread_local! {
    static REACTOR_THREAD: Cell<bool> = const { Cell::new(false) };
}

/// Marks the calling thread as the reactor's. Answers are recorded and
/// replayed only for questions asked from it.
pub fn mark_reactor_thread() {
    REACTOR_THREAD.with(|flag| flag.set(true));
}

fn on_reactor_thread() -> bool {
    REACTOR_THREAD.with(|flag| flag.get())
}

/// Begins writing a trace to `file`. The caller writes the header lines
/// through `write_line` before any event.
pub fn start_recording(file: File) {
    let mut mode = MODE.lock().unwrap_or_else(|e| e.into_inner());
    *mode = Mode::Record { file, started: Instant::now() };
    RECORDING.store(true, std::sync::atomic::Ordering::Release);
}

pub fn stop_recording() {
    let mut mode = MODE.lock().unwrap_or_else(|e| e.into_inner());
    if matches!(*mode, Mode::Record { .. }) {
        *mode = Mode::Off;
    }
    // The flight-recorder ring keeps running.
    RECORDING.store(true, std::sync::atomic::Ordering::Release);
}

pub fn is_recording() -> bool {
    RECORDING.load(std::sync::atomic::Ordering::Acquire)
}

/// Records activity from any rift thread — the event tap's decisions, the
/// app actors' accessibility traffic, raises, warps. `Act` lines are
/// documentation for a human (and the replay report); replay ignores them,
/// so instrumentation can never destabilize answer matching. Cheap when not
/// recording: one atomic load.
pub fn act<D: Serialize>(kind: &str, detail: &D) {
    if !is_recording() {
        return;
    }
    let thread = std::thread::current();
    let src = thread.name().unwrap_or("?").to_string();
    let detail = serde_json::to_string(detail).unwrap_or_default();
    let ms = elapsed_ms();
    write_line(&format!(
        "Act {{\"ms\":{ms},\"src\":{:?},\"kind\":{kind:?},\"detail\":{detail}}}",
        src
    ));
}

/// Milliseconds since the recording started (process start for the flight
/// ring). On replay, the timestamp of the line being replayed.
pub fn elapsed_ms() -> u64 {
    match &*MODE.lock().unwrap_or_else(|e| e.into_inner()) {
        Mode::Record { started, .. } => started.elapsed().as_millis() as u64,
        Mode::Replay { now_ms, .. } => *now_ms,
        Mode::Off => process_elapsed_ms(),
    }
}

/// Writes one raw line: to the file recording when one is running, else to
/// the flight-recorder ring.
pub fn write_line(line: &str) {
    if let Mode::Record { file, .. } = &mut *MODE.lock().unwrap_or_else(|e| e.into_inner()) {
        let _ = writeln!(file, "{line}");
        return;
    }
    let mut ring = RING.lock().unwrap_or_else(|e| e.into_inner());
    ring.bytes += line.len();
    ring.lines.push_back(line.to_string());
    while ring.bytes > RING_CAP_BYTES {
        let Some(evicted) = ring.lines.pop_front() else {
            break;
        };
        ring.bytes -= evicted.len();
    }
}

/// The reactor's clock. Real time when live; the recorded time on replay,
/// so holds and grace periods elapse exactly as they did.
pub fn now() -> Instant {
    match &*MODE.lock().unwrap_or_else(|e| e.into_inner()) {
        Mode::Replay { thread, base, now_ms, .. } if *thread == std::thread::current().id() => {
            *base + Duration::from_millis(*now_ms)
        }
        _ => Instant::now(),
    }
}

/// Asks the system a question, recording or replaying the answer.
///
/// `kind` names the question, `key` its arguments. On replay the next
/// recorded answer of the same kind and key is returned; if the recording
/// has none, `compute` runs (and the miss is reported) so a replay never
/// panics inside the reactor — the report is what fails the test.
pub fn observe<K, T>(kind: &str, key: K, compute: impl FnOnce() -> T) -> T
where
    K: Serialize,
    T: Serialize + DeserializeOwned,
{
    if !on_reactor_thread() {
        // Off-reactor system queries (the focus resolver, window-notify,
        // the event tap) used to vanish from recordings entirely — whole
        // debugging sessions were spent inferring what those threads did.
        // Record them as replay-inert `Act` lines instead.
        if is_recording() {
            let answer = compute();
            #[derive(Serialize)]
            struct OffThreadQuery<'a, K: Serialize, T: Serialize> {
                key: &'a K,
                answer: &'a T,
            }
            act(kind, &OffThreadQuery { key: &key, answer: &answer });
            return answer;
        }
        return compute();
    }
    let key_ron = serde_json::to_string(&key).unwrap_or_default();
    let mut mode = MODE.lock().unwrap_or_else(|e| e.into_inner());
    if let Mode::Replay { thread, .. } = &*mode
        && *thread != std::thread::current().id()
    {
        drop(mode);
        return compute();
    }
    match &mut *mode {
        Mode::Off => {
            // No file recording running: the answer still goes to the
            // flight-recorder ring, so a retroactive dump carries the same
            // Sys history a live recording would have.
            let ms = process_elapsed_ms();
            drop(mode);
            let answer = compute();
            let line = SysLine {
                ms,
                kind: kind.to_string(),
                key: key_ron,
                answer: serde_json::to_string(&answer).unwrap_or_default(),
            };
            if let Ok(json) = serde_json::to_string(&line) {
                write_line(&format!("Sys {json}"));
            }
            answer
        }
        Mode::Record { file: _, started } => {
            let ms = started.elapsed().as_millis() as u64;
            drop(mode);
            let answer = compute();
            let line = SysLine {
                ms,
                kind: kind.to_string(),
                key: key_ron,
                answer: serde_json::to_string(&answer).unwrap_or_default(),
            };
            if let Ok(json) = serde_json::to_string(&line) {
                write_line(&format!("Sys {json}"));
            }
            answer
        }
        Mode::Replay {
            answers,
            latest,
            misses,
            drifts,
            now_ms,
            ..
        } => {
            // The system's answer as of now: every recorded answer for this
            // question up to the current replay time is consumed, and the
            // last of them is the current state. Asked again before the
            // next recorded change, the same answer stands.
            let now = *now_ms;
            let slot = (kind.to_string(), key_ron.clone());
            let mut answer: Option<String> = None;
            if let Some(queue) = answers.get_mut(&slot) {
                while queue.front().is_some_and(|line| line.ms <= now) {
                    answer = queue.pop_front().map(|line| line.answer);
                }
            }
            let answer = answer.or_else(|| latest.get(&slot).cloned()).or_else(|| {
                // Not yet recorded at this time: the next recorded answer
                // for the key, or failing that the next of the kind.
                if let Some(line) = answers.get_mut(&slot).and_then(VecDeque::pop_front) {
                    return Some(line.answer);
                }
                let (other, _) = answers
                    .iter()
                    .filter(|((other_kind, _), queue)| other_kind == kind && !queue.is_empty())
                    .min_by_key(|(_, queue)| queue.front().map(|line| line.ms))
                    .map(|(slot, queue)| (slot.clone(), queue.len()))?;
                let line = answers.get_mut(&other)?.pop_front()?;
                drifts.push(format!("{kind}({key_ron}) answered by {}", line.key));
                Some(line.answer)
            });
            match answer {
                Some(json) => {
                    latest.insert((kind.to_string(), key_ron.clone()), json.clone());
                    match serde_json::from_str::<T>(&json) {
                        Ok(answer) => answer,
                        Err(error) => {
                            misses.push(format!("{kind}({key_ron}): undecodable answer: {error}"));
                            drop(mode);
                            compute()
                        }
                    }
                }
                None => {
                    misses.push(format!("{kind}({key_ron})"));
                    drop(mode);
                    compute()
                }
            }
        }
    }
}

/// Puts the process into replay mode with these recorded answers.
pub fn begin_replay(answers: Vec<SysLine>) {
    mark_reactor_thread();
    let mut mode = MODE.lock().unwrap_or_else(|e| e.into_inner());
    *mode = Mode::Replay {
        thread: std::thread::current().id(),
        answers: answers.into_iter().fold(
            std::collections::HashMap::new(),
            |mut queues: std::collections::HashMap<(String, String), VecDeque<SysLine>>, line| {
                queues.entry((line.kind.clone(), line.key.clone())).or_default().push_back(line);
                queues
            },
        ),
        latest: std::collections::HashMap::new(),
        base: Instant::now(),
        now_ms: 0,
        misses: Vec::new(),
        drifts: Vec::new(),
    };
}

/// Advances replayed time to the timestamp of the line about to be replayed.
pub fn replay_set_now(ms: u64) {
    if let Mode::Replay { now_ms, .. } = &mut *MODE.lock().unwrap_or_else(|e| e.into_inner()) {
        *now_ms = ms;
    }
}

/// Ends replay mode, returning the questions the recording could not answer.
pub fn end_replay() -> (Vec<String>, Vec<String>) {
    let mut mode = MODE.lock().unwrap_or_else(|e| e.into_inner());
    let result = match &mut *mode {
        Mode::Replay { misses, drifts, .. } => (std::mem::take(misses), std::mem::take(drifts)),
        _ => (Vec::new(), Vec::new()),
    };
    *mode = Mode::Off;
    result
}

/// Serializable stand-ins for CoreGraphics geometry.
pub type Rect = (f64, f64, f64, f64);
pub type Point = (f64, f64);

pub fn rect_to(r: objc2_core_foundation::CGRect) -> Rect {
    (r.origin.x, r.origin.y, r.size.width, r.size.height)
}
pub fn rect_from(r: Rect) -> objc2_core_foundation::CGRect {
    objc2_core_foundation::CGRect::new(
        objc2_core_foundation::CGPoint::new(r.0, r.1),
        objc2_core_foundation::CGSize::new(r.2, r.3),
    )
}
pub fn point_to(p: objc2_core_foundation::CGPoint) -> Point {
    (p.x, p.y)
}
pub fn point_from(p: Point) -> objc2_core_foundation::CGPoint {
    objc2_core_foundation::CGPoint::new(p.0, p.1)
}

/// A frame rift wrote to a window, as recorded live: the oracle a replay's
/// own writes are compared against.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OutLine {
    pub ms: u64,
    pub pid: i32,
    pub idx: u32,
    pub frame: Rect,
}

/// Records a frame write made to an app, from whichever thread makes it.
pub fn note_write(pid: i32, idx: u32, frame: objc2_core_foundation::CGRect) {
    if !is_recording() {
        return;
    }
    let line = OutLine {
        ms: elapsed_ms(),
        pid,
        idx,
        frame: rect_to(frame),
    };
    if let Ok(json) = serde_json::to_string(&line) {
        write_line(&format!("Out {json}"));
    }
}
