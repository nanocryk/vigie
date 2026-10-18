# Security

## Reporting a vulnerability

Report vulnerabilities privately through GitHub: on the repository's **Security** tab, click **Report a vulnerability**. Please don't open a public issue for them.

## Supported versions

Only the latest commit on `main` is supported. Fixes land there; there are no release branches.

## Threat model

vigie gives a paired device remote control of Claude Code on your machine. Its security comes down to keeping unpaired clients out, and to the approvals you give Claude.

### A paired device has full control of the machine

A paired device can do anything you can do at the machine:

- start Claude sessions in any folder, answer their permission asks, and switch them to Accept edits, which stops asking before file edits;
- browse and read any file your user account can read, through the file explorer;
- restart and update the server.

Pair only devices you control. Revoke a lost device from the device settings of another one, or revoke all devices from the machine with `vigie auth revoke-all`.

### What keeps other clients out

- **Pairing:** a device pairs with a single-use token. The one printed at startup or by `vigie auth pair` expires after 10 minutes; one issued from a paired device expires after 5. Tokens are 256-bit random values and travel in the URL fragment, which browsers never send to the server.
- **Device sessions:** a paired device holds an `HttpOnly`, `SameSite=Strict` cookie, valid for 60 days and checked on every request. Revoking a device ends its open connections.
- **Request checks:** requests whose `Host` is not an allowed origin are refused, and so are writes and WebSocket connections whose `Origin` is not. The allowed origins are loopback, the bind address, the machine's reachable addresses, `public_hosts` and `--allow-origin`.
- **TLS:** required. vigie refuses to start without a certificate, unless asked for a self-signed one (`--self-signed`) or plain HTTP (`--http`), which is refused on any address other than loopback. With the self-signed certificate, compare the fingerprint vigie prints at startup with the one the browser shows before accepting it.
- **Pairing rate limit:** after 5 failed pairing attempts in 10 minutes, the client's IP address is locked out for an hour.

### What the client protects against

Claude's replies, files and diffs are untrusted content: a repository can carry text written to manipulate Claude or you.

- The app never renders raw HTML from them, and a Content-Security-Policy limits what a page can load or run.
- Markdown images never load, so a reply can't leak data to another server just by being displayed.
- Approval cards show the full command, with hidden and direction-changing Unicode characters made visible.
- Opening a folder never runs programs named by that repository's own git config.

### Limits

- **Approvals are the boundary.** Once you allow a command, or put a session in Accept edits, Claude runs with your user's rights. vigie flags some dangerous asks (`rm -rf`, force pushes, `git reset --hard`, writes outside the session folder), but that check is a hint and misses commands. An ask without a warning is unflagged, not safe.
- **Exposure:** forwarding a port on your router puts vigie on the whole internet, with pairing and device sessions as the only barrier. Prefer a private network such as Tailscale (see the README).
- **Reverse proxies:** vigie sees the proxy's address, not the client's, so the pairing rate limit counts every client behind the proxy as one. A few failed attempts lock out pairing for all of them for an hour.
- **Push notifications ignore the system proxy:** vigie sends them straight to the browser's push service and refuses push endpoints that resolve to private or loopback addresses. On a network that only allows outgoing traffic through a proxy, notifications don't arrive.
