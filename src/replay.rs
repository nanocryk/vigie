//! Replays a recorded stream-json session, standing in for the `claude` CLI.
//!
//! A recording is the spike driver's ndjson: one `{"t", "dir", "msg"}` per line,
//! `out` for CLI stdout, `in` for CLI stdin, `note` for annotations. `out`
//! lines are written as recorded; an `in` line blocks until the host sends a
//! matching line.

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::path::Path;
use std::sync::LazyLock;
use std::time::Duration;

use anyhow::{Context, bail};
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, Lines};

/// Keys whose values legitimately differ between a recorded run and a replay.
const RUN_SPECIFIC_KEYS: [&str; 3] = ["session_id", "uuid", "timestamp"];
/// Longest rendering of a line inside an error message.
const SHOWN_CHARS: usize = 600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Host to CLI (stdin).
    In,
    /// CLI to host (stdout).
    Out,
}

#[derive(Debug, Clone)]
pub struct Step {
    /// 1-based line number in the recording file.
    pub line: usize,
    /// Milliseconds since the recorded launch.
    pub t: u64,
    pub dir: Direction,
    pub msg: Value,
}

#[derive(Debug, Clone)]
pub struct Recording {
    pub steps: Vec<Step>,
}

impl Recording {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("parse {}", path.display()))
    }

    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let mut steps = Vec::new();
        for (index, raw) in text.lines().enumerate() {
            let line = index + 1;
            if raw.trim().is_empty() {
                continue;
            }
            let mut record: Value =
                serde_json::from_str(raw).with_context(|| format!("line {line}: not JSON"))?;
            let dir = match record["dir"].as_str() {
                Some("in") => Direction::In,
                Some("out") => Direction::Out,
                Some("note") => continue,
                other => bail!("line {line}: unknown dir {other:?}"),
            };
            let t = record["t"].as_u64().unwrap_or(0);
            let msg = record["msg"].take();
            steps.push(Step { line, t, dir, msg });
        }
        Ok(Self { steps })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pace {
    /// Write `out` lines back to back.
    Instant,
    /// Keep the recorded delay between consecutive lines.
    Realtime,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub pace: Pace,
    /// Replaces the recorded `session_id` of every emitted line.
    pub session_id: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            pace: Pace::Instant,
            session_id: None,
        }
    }
}

/// The `get_usage` answer of the `basic` recording, served for any `get_usage`
/// the script does not expect.
static CANNED_USAGE: LazyLock<Value> = LazyLock::new(|| {
    let recording = Recording::parse(include_str!("../spike/runs/basic.ndjson"))
        .expect("basic recording parses");
    let request_id = recording
        .steps
        .iter()
        .find(|step| step.dir == Direction::In && is_get_usage(&step.msg))
        .and_then(|step| step.msg["request_id"].as_str())
        .expect("basic recording has a get_usage request");
    recording
        .steps
        .iter()
        .find(|step| {
            step.dir == Direction::Out
                && step.msg["type"] == "control_response"
                && step.msg["response"]["request_id"] == request_id
        })
        .map(|step| step.msg["response"]["response"].clone())
        .expect("basic recording answers get_usage")
});

fn is_get_usage(msg: &Value) -> bool {
    is_control_request(msg) && msg["request"]["subtype"] == "get_usage"
}

fn canned_usage_response(request_id: &Value) -> Value {
    serde_json::json!({"type": "control_response", "response": {
        "subtype": "success", "request_id": request_id, "response": CANNED_USAGE.clone()}})
}

#[derive(Debug)]
pub enum ReplayError {
    Mismatch {
        line: usize,
        expected: Value,
        got: String,
        difference: String,
    },
    UnexpectedEof {
        line: usize,
        expected: Value,
    },
    AfterEnd {
        got: String,
    },
    Io(std::io::Error),
}

impl fmt::Display for ReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mismatch {
                line,
                expected,
                got,
                difference,
            } => write!(
                f,
                "recording line {line}: host sent a different line ({difference})\n  expected: {}\n  got:      {}",
                shorten(&expected.to_string()),
                shorten(got),
            ),
            Self::UnexpectedEof { line, expected } => write!(
                f,
                "recording line {line}: stdin closed while waiting for {}",
                shorten(&expected.to_string()),
            ),
            Self::AfterEnd { got } => {
                write!(f, "recording ended but the host sent: {}", shorten(got))
            }
            Self::Io(err) => write!(f, "io: {err}"),
        }
    }
}

impl std::error::Error for ReplayError {}

impl From<std::io::Error> for ReplayError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

fn shorten(text: &str) -> String {
    if text.chars().count() <= SHOWN_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(SHOWN_CHARS).collect();
    format!("{head}...")
}

/// Plays `recording` against a host reading `output` and writing `input`, then
/// waits for `input` to close. Returns on the first mismatch.
pub async fn replay<R, W>(
    recording: &Recording,
    options: &Options,
    input: R,
    mut output: W,
) -> Result<(), ReplayError>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut host_lines = input.lines();
    // Recorded host request id -> id the host used in this run.
    let mut request_ids: HashMap<String, String> = HashMap::new();
    let mut clock = 0;
    for step in &recording.steps {
        match step.dir {
            Direction::Out => {
                if options.pace == Pace::Realtime && step.t > clock {
                    tokio::time::sleep(Duration::from_millis(step.t - clock)).await;
                }
                let mut msg = with_host_request_id(&step.msg, &request_ids);
                if let Some(session_id) = &options.session_id
                    && let Some(slot) = msg.get_mut("session_id")
                {
                    *slot = Value::String(session_id.clone());
                }
                write_line(&mut output, &msg).await?;
            }
            Direction::In => loop {
                let Some(got) = next_line(&mut host_lines).await? else {
                    return Err(ReplayError::UnexpectedEof {
                        line: step.line,
                        expected: step.msg.clone(),
                    });
                };
                match accept(step, got, &mut request_ids) {
                    Ok(()) => break,
                    Err(err) => answer_usage_or(err, &mut output).await?,
                }
            },
        }
        clock = step.t;
    }
    output.flush().await?;
    while let Some(got) = next_line(&mut host_lines).await? {
        answer_usage_or(ReplayError::AfterEnd { got: got.clone() }, &mut output).await?;
    }
    Ok(())
}

/// Answers an unscripted `get_usage` request; any other unexpected line is `err`.
async fn answer_usage_or<W: AsyncWrite + Unpin>(
    err: ReplayError,
    output: &mut W,
) -> Result<(), ReplayError> {
    let got = match &err {
        ReplayError::Mismatch { got, .. } | ReplayError::AfterEnd { got } => got,
        _ => return Err(err),
    };
    match serde_json::from_str::<Value>(got) {
        Ok(msg) if is_get_usage(&msg) => {
            write_line(output, &canned_usage_response(&msg["request_id"])).await
        }
        _ => Err(err),
    }
}

async fn write_line<W: AsyncWrite + Unpin>(output: &mut W, msg: &Value) -> Result<(), ReplayError> {
    let mut bytes = serde_json::to_vec(msg).map_err(std::io::Error::other)?;
    bytes.push(b'\n');
    output.write_all(&bytes).await?;
    output.flush().await?;
    Ok(())
}

async fn next_line<R: AsyncBufRead + Unpin>(
    lines: &mut Lines<R>,
) -> Result<Option<String>, ReplayError> {
    while let Some(line) = lines.next_line().await? {
        if !line.trim().is_empty() {
            return Ok(Some(line));
        }
    }
    Ok(None)
}

fn accept(
    step: &Step,
    got: String,
    request_ids: &mut HashMap<String, String>,
) -> Result<(), ReplayError> {
    let mismatch = |difference: String, got: String| ReplayError::Mismatch {
        line: step.line,
        expected: step.msg.clone(),
        got,
        difference,
    };
    let received: Value = match serde_json::from_str(&got) {
        Ok(value) => value,
        Err(err) => return Err(mismatch(format!("not JSON: {err}"), got)),
    };
    if let Some(difference) = first_difference(
        &comparable(&step.msg),
        &comparable(&received),
        String::new(),
    ) {
        return Err(mismatch(difference, got));
    }
    if is_control_request(&step.msg) {
        let recorded = step.msg["request_id"].as_str().unwrap_or_default();
        let host = received["request_id"].as_str().unwrap_or_default();
        request_ids.insert(recorded.to_string(), host.to_string());
    }
    Ok(())
}

fn is_control_request(msg: &Value) -> bool {
    msg["type"] == "control_request"
}

/// The recorded answer to a host request, carrying the id the host chose.
fn with_host_request_id(msg: &Value, request_ids: &HashMap<String, String>) -> Value {
    let mut msg = msg.clone();
    if msg["type"] == "control_response"
        && let Some(slot) = msg
            .get_mut("response")
            .and_then(|response| response.get_mut("request_id"))
        && let Some(host) = slot.as_str().and_then(|id| request_ids.get(id))
    {
        *slot = Value::String(host.clone());
    }
    msg
}

/// Drops what differs between runs: run-specific keys, null fields, and the id
/// of a host-originated control request.
fn comparable(msg: &Value) -> Value {
    let mut msg = msg.clone();
    if is_control_request(&msg)
        && let Some(object) = msg.as_object_mut()
    {
        object.remove("request_id");
    }
    strip(&mut msg);
    msg
}

fn strip(value: &mut Value) {
    match value {
        Value::Object(object) => {
            object.retain(|key, field| {
                !field.is_null() && !RUN_SPECIFIC_KEYS.contains(&key.as_str())
            });
            object.values_mut().for_each(strip);
        }
        Value::Array(items) => items.iter_mut().for_each(strip),
        _ => {}
    }
}

fn first_difference(expected: &Value, got: &Value, path: String) -> Option<String> {
    let at = |path: &str| if path.is_empty() { "/" } else { path }.to_string();
    match (expected, got) {
        (Value::Object(want), Value::Object(have)) => {
            let keys: BTreeSet<&String> = want.keys().chain(have.keys()).collect();
            keys.into_iter().find_map(|key| {
                let child = format!("{path}/{key}");
                match (want.get(key), have.get(key)) {
                    (Some(want), Some(have)) => first_difference(want, have, child),
                    (Some(_), None) => Some(format!("{child}: missing")),
                    _ => Some(format!("{child}: not in the recording")),
                }
            })
        }
        (Value::Array(want), Value::Array(have)) => want
            .iter()
            .zip(have)
            .enumerate()
            .find_map(|(index, (want, have))| {
                first_difference(want, have, format!("{path}/{index}"))
            })
            .or_else(|| {
                (want.len() != have.len()).then(|| {
                    format!(
                        "{}: expected {} items, got {}",
                        at(&path),
                        want.len(),
                        have.len()
                    )
                })
            }),
        _ if expected == got => None,
        _ => Some(format!(
            "{}: expected {}, got {}",
            at(&path),
            shorten(&expected.to_string()),
            shorten(&got.to_string()),
        )),
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{BufReader, DuplexStream, duplex};
    use tokio::task::JoinHandle;

    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(20);

    fn recording(name: &str) -> Recording {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("spike/runs")
            .join(format!("{name}.ndjson"));
        Recording::load(&path).expect("load recording")
    }

    struct Host {
        stdin: DuplexStream,
        stdout: Lines<BufReader<DuplexStream>>,
        fake: JoinHandle<Result<(), ReplayError>>,
    }

    impl Host {
        fn start(recording: Recording) -> Self {
            Self::start_with(recording, Options::default())
        }

        fn start_with(recording: Recording, options: Options) -> Self {
            let (stdin, fake_stdin) = duplex(1 << 16);
            let (fake_stdout, stdout) = duplex(1 << 16);
            let fake = tokio::spawn(async move {
                replay(
                    &recording,
                    &options,
                    BufReader::new(fake_stdin),
                    fake_stdout,
                )
                .await
            });
            Self {
                stdin,
                stdout: BufReader::new(stdout).lines(),
                fake,
            }
        }

        async fn send(&mut self, msg: &Value) {
            let mut line = serde_json::to_string(msg).unwrap();
            line.push('\n');
            self.stdin.write_all(line.as_bytes()).await.unwrap();
        }

        async fn receive(&mut self) -> Option<Value> {
            let line = tokio::time::timeout(TIMEOUT, self.stdout.next_line())
                .await
                .expect("fake claude went quiet")
                .unwrap()?;
            Some(serde_json::from_str(&line).unwrap())
        }

        async fn finish(self) -> Result<(), ReplayError> {
            drop(self.stdin);
            tokio::time::timeout(TIMEOUT, self.fake)
                .await
                .expect("fake claude did not exit")
                .unwrap()
        }
    }

    /// Plays the host side from the recording, with `rewrite_in` applied to every
    /// line sent and `rewrite_out` to every line expected back.
    async fn drive(
        name: &str,
        rewrite_in: impl Fn(&Value) -> Value,
        rewrite_out: impl Fn(&Value) -> Value,
    ) {
        drive_with(name, Options::default(), rewrite_in, rewrite_out).await;
    }

    async fn drive_with(
        name: &str,
        options: Options,
        rewrite_in: impl Fn(&Value) -> Value,
        rewrite_out: impl Fn(&Value) -> Value,
    ) {
        let recording = recording(name);
        let steps = recording.steps.clone();
        let mut host = Host::start_with(recording, options);
        let mut outs = 0;
        for step in &steps {
            match step.dir {
                Direction::In => host.send(&rewrite_in(&step.msg)).await,
                Direction::Out => {
                    let got = host
                        .receive()
                        .await
                        .unwrap_or_else(|| panic!("{name}:{}: stdout closed early", step.line));
                    assert_eq!(
                        got,
                        rewrite_out(&step.msg),
                        "{name}:{}: wrong line",
                        step.line
                    );
                    outs += 1;
                }
            }
        }
        assert!(outs > 0);
        host.finish().await.expect("replay failed");
    }

    #[tokio::test]
    async fn replays_basic() {
        drive("basic", Value::clone, Value::clone).await;
    }

    #[tokio::test]
    async fn replays_permissions() {
        drive("permissions", Value::clone, Value::clone).await;
    }

    #[tokio::test]
    async fn replays_interrupt() {
        drive("interrupt", Value::clone, Value::clone).await;
    }

    fn host_id(id: &str) -> String {
        id.replace("spike-", "host-")
    }

    /// The host picks its own request ids and session id; answers must carry the
    /// host's ids, and a `can_use_tool` answer still echoes the CLI's id.
    #[tokio::test]
    async fn host_request_ids_are_substituted() {
        let rewrite_in = |msg: &Value| {
            let mut msg = msg.clone();
            if let Some(id) = msg.get("request_id").and_then(Value::as_str) {
                msg["request_id"] = Value::String(host_id(id));
            }
            if msg.get("session_id").is_some() {
                msg["session_id"] = Value::String("another-session".into());
            }
            msg
        };
        let rewrite_out = |msg: &Value| {
            let mut msg = msg.clone();
            if msg["type"] == "control_response"
                && let Some(id) = msg["response"]["request_id"].as_str()
            {
                msg["response"]["request_id"] = Value::String(host_id(id));
            }
            msg
        };
        drive("interrupt", rewrite_in, rewrite_out).await;
    }

    #[tokio::test]
    async fn host_session_id_is_substituted() {
        let options = Options {
            pace: Pace::Instant,
            session_id: Some("host-session".into()),
        };
        let rewrite_out = |msg: &Value| {
            let mut msg = msg.clone();
            if msg.get("session_id").is_some() {
                msg["session_id"] = Value::String("host-session".into());
            }
            msg
        };
        drive_with("permissions", options, Value::clone, rewrite_out).await;
    }

    fn get_usage(request_id: &str) -> Value {
        serde_json::json!({"type": "control_request", "request_id": request_id,
            "request": {"subtype": "get_usage", "skip_behaviors": true}})
    }

    fn assert_usage_answer(got: &Value, request_id: &str) {
        assert_eq!(got["type"], "control_response");
        assert_eq!(got["response"]["request_id"], request_id);
        assert_eq!(got["response"]["response"]["rate_limits_available"], true);
    }

    #[tokio::test]
    async fn unscripted_get_usage_is_answered_without_consuming_the_script() {
        let recording = recording("interrupt");
        let steps = recording.steps.clone();
        let mut host = Host::start(recording);
        host.send(&get_usage("u1")).await;
        assert_usage_answer(&host.receive().await.expect("usage answer"), "u1");
        for step in &steps {
            match step.dir {
                Direction::In => host.send(&step.msg).await,
                Direction::Out => {
                    let got = host.receive().await.expect("out line");
                    assert_eq!(got, step.msg, "interrupt:{}", step.line);
                }
            }
        }
        host.send(&get_usage("u2")).await;
        assert_usage_answer(&host.receive().await.expect("usage answer"), "u2");
        host.finish().await.expect("replay failed");
    }

    #[tokio::test]
    async fn a_different_user_message_is_a_mismatch() {
        let recording = recording("basic");
        let steps = recording.steps.clone();
        let mut host = Host::start(recording);
        for step in &steps {
            match step.dir {
                Direction::Out => {
                    host.receive().await.expect("out line");
                }
                Direction::In if step.msg["type"] == "user" => {
                    let mut wrong = step.msg.clone();
                    wrong["message"]["content"] = Value::String("something else".into());
                    host.send(&wrong).await;
                    break;
                }
                Direction::In => host.send(&step.msg).await,
            }
        }
        let err = host.finish().await.expect_err("mismatch accepted");
        let ReplayError::Mismatch {
            line, difference, ..
        } = &err
        else {
            panic!("expected a mismatch, got {err}");
        };
        assert_eq!(*line, 8);
        assert!(
            difference.starts_with("/message/content: expected"),
            "{difference}"
        );
        assert!(err.to_string().contains("recording line 8"), "{err}");
    }

    #[tokio::test]
    async fn closing_stdin_early_is_an_error() {
        let host = Host::start(recording("basic"));
        let err = host.finish().await.expect_err("eof accepted");
        assert!(
            matches!(err, ReplayError::UnexpectedEof { line: 2, .. }),
            "{err}"
        );
    }

    #[test]
    fn run_specific_fields_are_ignored() {
        let recorded = serde_json::json!({"type": "user", "session_id": "a",
            "parent_tool_use_id": null, "message": {"role": "user", "content": "hi"}});
        let sent = serde_json::json!({"type": "user", "session_id": "b",
            "message": {"role": "user", "content": "hi"}});
        assert_eq!(
            first_difference(&comparable(&recorded), &comparable(&sent), String::new()),
            None
        );
    }
}
