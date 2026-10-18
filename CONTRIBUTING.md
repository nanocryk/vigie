# Contributing

## Layout

- `src/`: the Rust server.
- `web/`: the React and TypeScript client, built into `web/dist`, which the server serves.
- `spike/`: the recorded Claude Code sessions the tests replay, and the driver that recorded them.

## Build and test

- Server: `cargo test` runs the suite against a fake `claude` that replays the recordings in `spike/runs`, so tests spend no usage. `cargo clippy --all-targets` should stay clean, and `cargo fmt` formats.
- Client: from `web/`, `npm run check` type-checks and runs the tests, `npm run build` builds `web/dist`, and `npm run format` formats.
- CI runs all of these on every push and pull request (`.github/workflows/ci.yml`): the Rust checks on Windows, Linux and macOS, plus a check with Rust 1.89, the oldest supported. A weekly job audits the dependencies.

## Design documents

- `DESIGN.md`: what vigie does and why, feature by feature.
- `notes/protocol.md`: how headless Claude Code talks (stream-json, permission asks, hooks), with what was observed live.
- `notes/wire.md`: the protocol between the server and its clients, WebSocket and REST.
- `notes/restart-advice.md`: when the app advises restarting a discussion, and how the saving is computed.

Update the design document a change touches in the same commit, describing what the code does now.

## Security

See [SECURITY.md](SECURITY.md) for the threat model and how to report a vulnerability.
