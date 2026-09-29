//! Replays a recorded stream-json session, standing in for the `claude` CLI.
//!
//! A recording is the spike driver's ndjson: one `{"t", "dir", "msg"}` per line,
//! `out` for CLI stdout, `in` for CLI stdin, `note` for annotations. `out`
//! lines are written as recorded; an `in` line blocks until the host sends a
//! matching line. Unscripted `get_usage`, `get_context_usage`, `get_settings`,
//! `set_model` and `apply_flag_settings` requests are answered without
//! consuming the script.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use anyhow::{Context, bail};
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, Lines};

use crate::claude::protocol::Inbound;

/// Keys whose values legitimately differ between a recorded run and a replay.
const RUN_SPECIFIC_KEYS: [&str; 3] = ["session_id", "uuid", "timestamp"];
/// Keys the host sends that older recordings predate; compared only when recorded.
const LATER_HOST_KEYS: [&str; 1] = ["sdkMcpServers"];
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

/// The folder of the spike driver's recordings.
pub fn runs_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("spike/runs")
}

/// The CLI stdout lines of recording `name`, typed.
pub fn recorded_lines(name: &str) -> anyhow::Result<Vec<Inbound>> {
    Recording::named(name)?
        .out()
        .map(|msg| Ok(serde_json::from_value(msg.clone())?))
        .collect()
}

impl Recording {
    /// `spike/runs/<name>.ndjson`.
    pub fn path(name: &str) -> PathBuf {
        runs_dir().join(format!("{name}.ndjson"))
    }

    pub fn named(name: &str) -> anyhow::Result<Self> {
        Self::load(&Self::path(name))
    }

    /// The CLI stdout lines, in order.
    pub fn out(&self) -> impl Iterator<Item = &Value> {
        self.steps
            .iter()
            .filter(|step| step.dir == Direction::Out)
            .map(|step| &step.msg)
    }

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
    /// Sets `current_permission_mode` in the answer to the `initialize` request,
    /// as the CLI reports its `--permission-mode`.
    pub permission_mode: Option<String>,
    /// `--model` and `--effort`, reported by `get_settings` until changed.
    pub model: Option<String>,
    pub effort: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            pace: Pace::Instant,
            session_id: None,
            permission_mode: None,
            model: None,
            effort: None,
        }
    }
}

/// Recorded request ids of the host's `initialize` requests.
fn initialize_ids(recording: &Recording) -> HashSet<String> {
    recording
        .steps
        .iter()
        .filter(|step| {
            step.dir == Direction::In
                && is_control_request(&step.msg)
                && step.msg["request"]["subtype"] == "initialize"
        })
        .filter_map(|step| step.msg["request_id"].as_str().map(str::to_string))
        .collect()
}

/// The `get_usage` answer of the `basic` recording, served for any `get_usage`
/// the script does not expect.
static CANNED_USAGE: LazyLock<Value> = LazyLock::new(|| {
    let recording = Recording::parse(include_str!("../../spike/runs/basic.ndjson"))
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
    success_response(request_id, CANNED_USAGE.clone())
}

fn success_response(request_id: &Value, payload: Value) -> Value {
    serde_json::json!({"type": "control_response", "response": {
        "subtype": "success", "request_id": request_id, "response": payload}})
}

/// Context total of a replay before its first main request.
pub const CANNED_CONTEXT_START: u64 = 12_000;
pub const CANNED_CONTEXT_MAX: u64 = 967_000;
pub const CANNED_AUTOCOMPACT_THRESHOLD: u64 = 900_000;
const CANNED_SYSTEM_TOKENS: u64 = 3_000;
const PROMPT_KEYS: [&str; 3] = [
    "input_tokens",
    "cache_read_input_tokens",
    "cache_creation_input_tokens",
];

fn is_get_context_usage(msg: &Value) -> bool {
    is_control_request(msg) && msg["request"]["subtype"] == "get_context_usage"
}

fn is_settings_change(msg: &Value) -> bool {
    is_control_request(msg)
        && (msg["request"]["subtype"] == "set_model"
            || msg["request"]["subtype"] == "apply_flag_settings")
}

fn is_get_settings(msg: &Value) -> bool {
    is_control_request(msg) && msg["request"]["subtype"] == "get_settings"
}

/// Model the replay reports applying when no `--model` or `set_model` chose one.
pub const CANNED_DEFAULT_MODEL: &str = "claude-opus-5-5[1m]";
/// Effort the replay reports applying when no `--effort` or effort change chose one.
pub const CANNED_DEFAULT_EFFORT: &str = "medium";

/// The model and effort the replay reports applying, following `--model`,
/// `--effort` and the settings changes the host sends.
#[derive(Debug, Clone)]
struct FakeSettings {
    model: Option<String>,
    effort: Option<String>,
}

impl FakeSettings {
    fn new(options: &Options) -> Self {
        Self {
            model: options.model.clone(),
            effort: options.effort.clone(),
        }
    }

    fn apply(&mut self, msg: &Value) {
        let request = &msg["request"];
        if request["subtype"] == "set_model" {
            self.model = request["model"].as_str().map(str::to_string);
        } else if let Some(effort) = request["settings"]["effortLevel"].as_str() {
            self.effort = Some(effort.to_string());
        }
    }

    /// A hand-written `get_settings` reply, the CLI's aliases resolved as the
    /// `basic` recording's `initialize` lists them.
    fn reply(&self) -> Value {
        let model = match self.model.as_deref() {
            None | Some("default" | "opus") => CANNED_DEFAULT_MODEL,
            Some("sonnet") => "claude-sonnet-5",
            Some("haiku") => "claude-haiku-4-5-20251001",
            Some(other) => other,
        };
        let effort = self.effort.as_deref().unwrap_or(CANNED_DEFAULT_EFFORT);
        serde_json::json!({
            "effective": {},
            "sources": [],
            "applied": {"model": model, "effort": effort},
        })
    }
}

fn prompt_tokens(usage: &Value) -> u64 {
    PROMPT_KEYS
        .iter()
        .map(|key| usage[key].as_u64().unwrap_or(0))
        .sum()
}

/// The usage of a main-conversation request an `out` line reports, if any.
fn main_request_usage(msg: &Value) -> Option<&Value> {
    let usage = &msg["message"]["usage"];
    (msg["type"] == "assistant" && msg["parent_tool_use_id"].is_null() && prompt_tokens(usage) > 0)
        .then_some(usage)
}

/// A hand-written `get_context_usage` reply whose total is the prompt of the
/// last main request played, as the CLI reports it.
fn canned_context_usage(last_usage: Option<&Value>) -> Value {
    let total = last_usage.map_or(CANNED_CONTEXT_START, prompt_tokens);
    let api_usage = last_usage.map_or(Value::Null, |usage| {
        serde_json::json!({
            "input_tokens": usage["input_tokens"],
            "output_tokens": usage["output_tokens"],
            "cache_creation_input_tokens": usage["cache_creation_input_tokens"],
            "cache_read_input_tokens": usage["cache_read_input_tokens"],
        })
    });
    serde_json::json!({
        "totalTokens": total,
        "maxTokens": CANNED_CONTEXT_MAX,
        "rawMaxTokens": 1_000_000,
        "percentage": (total * 100 + CANNED_CONTEXT_MAX / 2) / CANNED_CONTEXT_MAX,
        "model": "claude-opus-5-5[1m]",
        "autoCompactThreshold": CANNED_AUTOCOMPACT_THRESHOLD,
        "isAutoCompactEnabled": true,
        "apiUsage": api_usage,
        "categories": [
            {"name": "System prompt", "tokens": CANNED_SYSTEM_TOKENS, "kind": "used"},
            {"name": "Messages", "tokens": total.saturating_sub(CANNED_SYSTEM_TOKENS), "kind": "used"},
            {"name": "Free space", "tokens": CANNED_CONTEXT_MAX.saturating_sub(total), "kind": "free"},
        ],
    })
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
    let mut last_usage: Option<Value> = None;
    let mut settings = FakeSettings::new(options);
    let initialize = initialize_ids(recording);
    for step in &recording.steps {
        match step.dir {
            Direction::Out => {
                if options.pace == Pace::Realtime && step.t > clock {
                    tokio::time::sleep(Duration::from_millis(step.t - clock)).await;
                }
                if let Some(usage) = main_request_usage(&step.msg) {
                    last_usage = Some(usage.clone());
                }
                let mut msg = with_host_request_id(&step.msg, &request_ids);
                if let Some(session_id) = &options.session_id
                    && let Some(slot) = msg.get_mut("session_id")
                {
                    *slot = Value::String(session_id.clone());
                }
                if let Some(mode) = &options.permission_mode
                    && step.msg["type"] == "control_response"
                    && step.msg["response"]["request_id"]
                        .as_str()
                        .is_some_and(|id| initialize.contains(id))
                    && let Some(response) = msg["response"]["response"].as_object_mut()
                {
                    response.insert(
                        "current_permission_mode".into(),
                        Value::String(mode.clone()),
                    );
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
                    Err(err) => {
                        answer_unscripted_or(err, last_usage.as_ref(), &mut settings, &mut output)
                            .await?
                    }
                }
            },
        }
        clock = step.t;
    }
    output.flush().await?;
    while let Some(got) = next_line(&mut host_lines).await? {
        let err = ReplayError::AfterEnd { got: got.clone() };
        answer_unscripted_or(err, last_usage.as_ref(), &mut settings, &mut output).await?;
    }
    Ok(())
}

/// Answers an unscripted `get_usage`, `get_context_usage`, `get_settings`,
/// `set_model` or `apply_flag_settings` request; any other unexpected line is `err`.
async fn answer_unscripted_or<W: AsyncWrite + Unpin>(
    err: ReplayError,
    last_usage: Option<&Value>,
    settings: &mut FakeSettings,
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
        Ok(msg) if is_get_context_usage(&msg) => {
            let reply = success_response(&msg["request_id"], canned_context_usage(last_usage));
            write_line(output, &reply).await
        }
        Ok(msg) if is_get_settings(&msg) => {
            write_line(
                output,
                &success_response(&msg["request_id"], settings.reply()),
            )
            .await
        }
        Ok(msg) if is_settings_change(&msg) => {
            settings.apply(&msg);
            write_line(output, &success_response(&msg["request_id"], Value::Null)).await
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
                    (None, Some(_)) if LATER_HOST_KEYS.contains(&key.as_str()) => None,
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
        Recording::named(name).expect("load recording")
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
            ..Options::default()
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

    #[tokio::test]
    async fn launch_permission_mode_is_reported_by_initialize() {
        let options = Options {
            permission_mode: Some("plan".into()),
            ..Options::default()
        };
        let rewrite_out = |msg: &Value| {
            let mut msg = msg.clone();
            if msg["type"] == "control_response" && msg["response"]["request_id"] == "spike-1" {
                msg["response"]["response"]["current_permission_mode"] = "plan".into();
            }
            msg
        };
        drive_with("basic", options, Value::clone, rewrite_out).await;
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

    fn get_context_usage(request_id: &str) -> Value {
        serde_json::json!({"type": "control_request", "request_id": request_id,
            "request": {"subtype": "get_context_usage"}})
    }

    #[tokio::test]
    async fn unscripted_get_context_usage_reports_the_last_main_prompt() {
        let recording = recording("basic");
        let steps = recording.steps.clone();
        let mut host = Host::start(recording);
        host.send(&get_context_usage("c1")).await;
        let before = host.receive().await.expect("context answer");
        assert_eq!(before["response"]["request_id"], "c1");
        let reply = &before["response"]["response"];
        assert_eq!(reply["totalTokens"], CANNED_CONTEXT_START);
        assert_eq!(reply["maxTokens"], CANNED_CONTEXT_MAX);
        assert!(reply["apiUsage"].is_null());
        for step in &steps {
            match step.dir {
                Direction::In => host.send(&step.msg).await,
                Direction::Out => {
                    host.receive().await.expect("out line");
                }
            }
        }
        host.send(&get_context_usage("c2")).await;
        let after = host.receive().await.expect("context answer");
        // input 2, cache read 29004, cache creation 1979.
        assert_eq!(after["response"]["response"]["totalTokens"], 30_985);
        assert_eq!(
            after["response"]["response"]["apiUsage"]["cache_read_input_tokens"],
            29_004
        );
        host.finish().await.expect("replay failed");
    }

    fn control(request_id: &str, request: Value) -> Value {
        serde_json::json!({"type": "control_request", "request_id": request_id,
            "request": request})
    }

    async fn applied(host: &mut Host, request_id: &str) -> Value {
        host.send(&control(
            request_id,
            serde_json::json!({"subtype": "get_settings"}),
        ))
        .await;
        let reply = host.receive().await.expect("settings answer");
        assert_eq!(reply["response"]["request_id"], request_id);
        reply["response"]["response"]["applied"].clone()
    }

    #[tokio::test]
    async fn unscripted_get_settings_follows_the_launch_flags_and_changes() {
        let options = Options {
            effort: Some("high".into()),
            ..Options::default()
        };
        let mut host = Host::start_with(recording("basic"), options);
        assert_eq!(
            applied(&mut host, "s1").await,
            serde_json::json!({"model": CANNED_DEFAULT_MODEL, "effort": "high"})
        );
        host.send(&control(
            "m1",
            serde_json::json!({"subtype": "set_model", "model": "sonnet"}),
        ))
        .await;
        host.receive().await.expect("set_model answer");
        host.send(&control(
            "e1",
            serde_json::json!({"subtype": "apply_flag_settings",
                "settings": {"effortLevel": "low"}}),
        ))
        .await;
        host.receive().await.expect("apply_flag_settings answer");
        assert_eq!(
            applied(&mut host, "s2").await,
            serde_json::json!({"model": "claude-sonnet-5", "effort": "low"})
        );
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

    #[test]
    fn later_host_keys_are_compared_only_when_recorded() {
        let old = serde_json::json!({"request": {"subtype": "initialize"}});
        let new = serde_json::json!({"request": {"subtype": "initialize", "sdkMcpServers": ["a"]}});
        let other =
            serde_json::json!({"request": {"subtype": "initialize", "sdkMcpServers": ["b"]}});
        assert_eq!(first_difference(&old, &new, String::new()), None);
        assert!(first_difference(&new, &old, String::new()).is_some());
        assert!(first_difference(&new, &other, String::new()).is_some());
    }
}
