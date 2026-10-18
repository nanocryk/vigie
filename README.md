# vigie

A web app, reachable from a phone or a computer, that drives Claude Code on your own machine. The server runs `claude -p` headlessly with stream-json input and output and renders a native chat UI around it. No terminal in the loop.

## Vision

Supervised work with Claude Code, away from the desk as much as at it. You read and steer; Claude works on your machine, with your own Claude Code instructions, skills and tools.

- **Reliability first.** Messages are never lost in transit, stop really stops, and the host keeps its own history of every discussion.
- **Code stays readable.** Diffs are shown the way a developer reads them, never hidden.
- **Nothing captures input by surprise.** An approval never steals a keystroke meant for something else, and risky answers take a deliberate gesture.
- **Calm and legible.** Designed for a neurodivergent brain: clear states, no clutter, no cold corporate feel.

## Features

- **Chat view**: streamed replies in markdown, compact tool rows, Edit and Write diffs with syntax highlighting, subagents each in their own view.
- **Approvals by swipe**: permission asks, questions and plans come as cards; a double swipe answers, with notes, per-file grants and a cooldown on dangerous asks. Buttons do the same on desktop.
- **Plan review**: plans render as markdown, with word-level highlights of what changed since the previous version.
- **Usage and cache gauges**: 5-hour and 7-day account windows with projections, context usage, and a cache bar draining over its TTL. A cold cache blocks sending until you choose to rewarm it, continue in a fresh session seeded from history, or start blank.
- **Session controls**: permission mode, model, effort and slash commands from a menu, with swipe gestures on the send button.
- **Several devices at once**: a card answered on one device disappears on the others. Push notifications when Claude needs you or a cache is about to go cold.
- **File explorer and diff view**: browse the machine, start a session in any folder, view a repository's pending changes.
- **Home**: every session with its state, pulsing when it needs attention; kill, archive and restore by swipe.

## Installation

### Requirements

- [Rust](https://rustup.rs/) 1.85 or newer (edition 2024).
- [Node.js](https://nodejs.org/) with npm, to build the web app.
- [Claude Code](https://docs.claude.com/en/docs/claude-code) installed, with `claude` on the `PATH`, and logged in with a Claude subscription. vigie refuses to run sessions billed through an API key: it removes `ANTHROPIC_API_KEY` from Claude's environment, and stops a session that reports any other key source.

### Build

From the repository root, build the web app into `web/dist`:

```sh
cd web
npm ci
npm run build
cd ..
```

Rebuild it after every update: the server does not rebuild it.

### Run

From the repository root, build and start the server:

```sh
./serve.sh
```

Its arguments go to the server, e.g. `./serve.sh --bind 0.0.0.0:7879`. It restarts the server when asked from the app. On Windows, run `serve.bat` instead, from any terminal or by double-clicking it: it runs `serve.sh` with the bash of [Git for Windows](https://gitforwindows.org/).

By default it listens on `https://127.0.0.1:7879/vigie/` with a self-signed certificate, and prints a pair URL:

```
vigie listening on https://127.0.0.1:7879/vigie/
Pair URL (single use, expires in 10 minutes; `vigie auth pair` prints a new one):
  https://127.0.0.1:7879/vigie/#auth=...
```

Open the pair URL in a browser to pair that device. Once it expires, print a fresh one for the running server, also single use for 10 minutes:

```sh
cargo run --release -- auth pair
```

The browser warns about the self-signed certificate on first visit. Before accepting it, compare the SHA-256 fingerprint the browser shows with the one vigie prints at startup: a match means nothing sits between the browser and vigie. Once one device is paired, pair the others from its device settings (the cogwheel at the top right): pick the address the link should use, then scan the QR code or copy the link. That link is single use and expires after 5 minutes.

### Configuration

Settings live in `~/.vigie/config.json`, next to the database, keys and device registry. Every field is optional, and a command-line flag overrides its field.

| Field           | Flag                     | Default          | Meaning                                                  |
|-----------------|--------------------------|------------------|----------------------------------------------------------|
| `bind`          | `--bind`                 | `127.0.0.1:7879` | Address and port to listen on.                           |
| `cert`, `key`   | `--cert`, `--key`        | self-signed      | TLS certificate and private key (PEM), given together.   |
| `public_hosts`  | `--public-host` (repeat) | none             | Extra `host[:port]` names offered for pairing and accepted as origins. |
| `allow_origins` | `--allow-origin` (repeat)| none             | Extra origins accepted on the WebSocket connection, on writes, and as the Host. |

Other flags: `--http` disables TLS, and is refused on any address other than loopback. `--no-print-token` skips the pair URL.

To revoke every paired device at once, including on a running server:

```sh
cargo run --release -- auth revoke-all
```

## Remote access with Tailscale (recommended)

[Tailscale](https://tailscale.com/) puts your phone and your machine on a private network (a tailnet), and can issue a real TLS certificate for the machine's tailnet name. This is the recommended setup: nothing is exposed to the internet, and the phone trusts the certificate, so there is no warning page, and the installed app and push notifications work.

1. Install Tailscale on the machine running vigie and on the phone, logged in to the same tailnet.
2. In the Tailscale admin console, on the DNS page, enable MagicDNS and HTTPS Certificates.
3. Find the machine's full tailnet name, for example `mybox.tail1234.ts.net`: it is listed on the Machines page of the admin console.
4. Get a certificate for that name. `tailscale cert` writes it to the current directory, so run it from `~/.vigie`:

   ```sh
   cd ~/.vigie
   tailscale cert mybox.tail1234.ts.net
   ```

   This writes `mybox.tail1234.ts.net.crt` and `mybox.tail1234.ts.net.key`.

5. Point vigie at them in `~/.vigie/config.json`. Binding to the machine's Tailscale IP (shown by `tailscale ip -4`) makes vigie reachable only through the tailnet; bind `0.0.0.0:7879` instead to also reach it on the local network.

   ```json
   {
     "bind": "100.101.102.103:7879",
     "cert": "C:\\Users\\me\\.vigie\\mybox.tail1234.ts.net.crt",
     "key": "C:\\Users\\me\\.vigie\\mybox.tail1234.ts.net.key",
     "public_hosts": ["mybox.tail1234.ts.net"]
   }
   ```

   A public host without a port gets the bind port.

6. Start vigie. The startup output now includes a pair URL on `https://mybox.tail1234.ts.net:7879/vigie/`; open it on the phone, or pair from an already paired device and pick the tailnet name.

The certificate is valid for 90 days. Renew it by running `tailscale cert` again, then restart vigie: the certificate is loaded at startup. Certificates are published in public Certificate Transparency logs, so the tailnet name becomes public, though the machine stays unreachable outside the tailnet.

## Remote access with port forwarding

Without Tailscale, forward a port on the router to the machine and bind vigie to `0.0.0.0`. This exposes vigie to the whole internet: the pairing token and device authentication are then all that stand between the internet and a Claude Code able to run commands on your machine. Prefer Tailscale.

- Bring your own certificate for a domain name pointing at the router (`cert` and `key`), and add that name to `public_hosts`. With the self-signed certificate, the phone shows a warning on every new visit, and the installed app and push notifications may not work.
- Keep the forwarded port the same as the bind port, or give the public host its port explicitly (`"example.com:443"`).

## Development

- `web/`: the React and TypeScript client (`npm run build`, `npm run check` for the type-check and tests).
- `src/`: the Rust server. `cargo test` runs the suite against a fake `claude` that replays recorded sessions from `spike/runs`, so tests spend no usage.
- `DESIGN.md` holds the design, `notes/` the protocol research and the client wire protocol.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option.
