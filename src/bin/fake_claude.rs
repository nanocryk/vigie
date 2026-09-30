//! Stand-in for the `claude` CLI that replays a spike recording.
//!
//! Usage: `fake-claude [--realtime] <recording.ndjson> [claude flags...]`. The
//! first argument other than `--realtime` is the recording; the rest is the
//! real CLI's argv. Its `--session-id` or `--resume` value replaces the
//! recorded session id, and its `--permission-mode` value is reported as the
//! `initialize` response's `current_permission_mode`, and its `--model` and
//! `--effort` values by `get_settings`; other flags are ignored.
//! The CLI flags are echoed on stderr as `fake-claude args: <flags>`.

use std::path::PathBuf;
use std::process::ExitCode;

use tokio::io::BufReader;
use vigie::claude::replay::{Options, Pace, Recording, replay};

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let mut options = Options::default();
    let mut recording_path: Option<PathBuf> = None;
    let mut flags = Vec::new();
    for arg in std::env::args().skip(1) {
        if arg == "--realtime" {
            options.pace = Pace::Realtime;
        } else if recording_path.is_none() {
            recording_path = Some(arg.into());
        } else {
            flags.push(arg);
        }
    }
    for pair in flags.windows(2) {
        let [flag, value] = pair else { continue };
        if flag == "--session-id" || flag == "--resume" {
            options.session_id = Some(value.clone());
        } else if flag == "--permission-mode" {
            options.permission_mode = Some(value.clone());
        } else if flag == "--model" {
            options.model = Some(value.clone());
        } else if flag == "--effort" {
            options.effort = Some(value.clone());
        }
    }
    eprintln!("fake-claude args: {}", flags.join(" "));
    let Some(recording_path) = recording_path else {
        eprintln!("usage: fake-claude [--realtime] <recording.ndjson> [claude flags...]");
        return ExitCode::from(2);
    };
    let recording = match Recording::load(&recording_path) {
        Ok(recording) => recording,
        Err(err) => {
            eprintln!("fake-claude: {err:#}");
            return ExitCode::from(2);
        }
    };
    let stdin = BufReader::new(tokio::io::stdin());
    match replay(&recording, &options, stdin, tokio::io::stdout()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("fake-claude: {err}");
            ExitCode::FAILURE
        }
    }
}
