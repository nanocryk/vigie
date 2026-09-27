//! Stand-in for the `claude` CLI that replays a spike recording.
//!
//! Usage: `fake-claude [--realtime] <recording.ndjson> [claude flags...]`. The
//! first argument other than `--realtime` is the recording; the rest is the
//! real CLI's argv, accepted and ignored.

use std::path::PathBuf;
use std::process::ExitCode;

use tokio::io::BufReader;
use vigie2::replay::{Pace, Recording, replay};

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let mut pace = Pace::Instant;
    let mut recording_path: Option<PathBuf> = None;
    for arg in std::env::args().skip(1) {
        if arg == "--realtime" {
            pace = Pace::Realtime;
        } else if recording_path.is_none() {
            recording_path = Some(arg.into());
        }
    }
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
    match replay(&recording, pace, stdin, tokio::io::stdout()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("fake-claude: {err}");
            ExitCode::FAILURE
        }
    }
}
