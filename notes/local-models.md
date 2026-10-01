# Local models

Research notes (2026-10-01). Nothing here is decided.

## Facts

- Runtimes (Ollama, llama.cpp `llama-server`, LM Studio, vLLM) serve open-weight models over HTTP. Ollama 0.14+ speaks the Anthropic Messages API (streaming, tool calls, thinking).
- The `claude` CLI talks to any Messages-compatible server: `ANTHROPIC_BASE_URL=http://<host>:11434`, `ANTHROPIC_AUTH_TOKEN=ollama`, `ANTHROPIC_API_KEY=""`, `--model <backend model name>`. Quick test: `ollama launch claude`. Not supported by Anthropic: works because the wire format matches.
- Lost with a non-Anthropic backend: server-side tools (WebSearch), prompt caching and its TTL, usage limits (`get_usage`, `rate_limit_event`).
- Hosted Anthropic-compatible endpoints exist too: Moonshot Kimi, Z.ai GLM, DeepSeek, OpenRouter.
- The model can run on another machine: Ollama listens on `127.0.0.1` by default, `OLLAMA_HOST=0.0.0.0:11434` opens it. No auth: LAN or tailnet only.
- Claude Code needs 64k+ context. KV cache memory comes on top of the weights. Its large system prompt makes prefill slow on a local GPU at each new session.
- OpenClaw: a messaging gateway daemon (WhatsApp, Telegram, Slack, WebSocket API, JSONL sessions) embedding the Pi agent (`badlogic/pi-mono`: 4 tools, system prompt under 1k tokens, multi-provider). Similar harnesses: OpenCode, Codex CLI `--oss`, Goose, Aider.

## Qwen sizing (from 2026 hardware guides, untested)

| Memory | Model | Q4 size |
|---|---|---|
| 24 GB | Qwen3.6-27B dense (77.2 SWE-bench Verified reported) | ~17 GB |
| 24 GB | Qwen3.5/3.6 35B-A3B MoE | ~20 GB |
| 64 GB+ unified, or 24-32 GB GPU + 64 GB RAM | Qwen3-Coder-Next 80B-A3B MoE, agent-tuned, 256k context | ~52 GB |
| 8 GB | Qwen3.5 9B: classification and extraction, not agent work | |

## Integration levels in vigie

- Level 1: same `claude -p` child with the env above. Today `src/claude/child.rs` strips `ANTHROPIC_AUTH_TOKEN`. Needs a per-session backend choice and hiding the cache gauge, cold-cache gate, cache push, usage windows and rewarm estimate for local sessions. Risk: undocumented CLI behaviour.
- Level 2: another harness (Pi RPC, OpenCode server, Codex app-server) behind a new adapter beside `src/claude/`. `src/session/` assumes Claude's control protocol: close to a rewrite.

## Idea: small models preprocess, Claude decides

Small local model for high-volume low-stakes work (extract, classify, summarize), Claude for judgment over the result.

Candidate uses in vigie:

- Discussion titles and "while you were away" summaries on Home.
- Push notification text saying what the ask is.
- Brief of older history added to the continue seed.
- Semantic search over history with a local embedding model.
- Grouping recurring Bash asks (`src/store/bash_asks.rs`) into proposed canif scripts or allow rules.
- Local Whisper for phone dictation.
- Daily journal prose: conflicts with DESIGN.md "built from history without a model call".

Rules of thumb:

- Rank and summarize, never discard: a wrongly dropped item is invisible.
- Keep the raw source one click away; summaries drop the line that matters (CI logs).
- No decisions with consequences (permission answers, danger checks).

## Email filtering

First use case the user named.

- Fetch: Proton Mail Bridge (paid plan) exposes local IMAP; IMAP IDLE for new mail.
- Clean: MIME parse, HTML to text, cut quotes and signatures, truncate.
- Classify: Ollama with a JSON schema (`format`), e.g. `{category, priority, needs_reply, deadline, project, one_line_summary}`. A 9B model on CPU or a small GPU suffices.
- Act: move or label over IMAP, never delete.
- Output: important items plus a daily digest for the user or Claude.
- Proton Sieve filters handle known senders deterministically before the LLM.
- Prompt injection: the classifier has no tools and its output is held to the schema. A Claude session reading the digest gets no dangerous tools without approval.
- Shadow mode first: classify without moving, compare with the user's own sorting.
- A separate service, not part of vigie; it could expose the digest to vigie through a file or an MCP server.

## Open

- Daily email volume and share of noise.
- Hardware of a GPU machine.
- Whether any vigie use above is wanted.
