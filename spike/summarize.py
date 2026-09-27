"""Print the protocol fields the spike checks, from runs/<scenario>.ndjson."""

import json
import sys
from pathlib import Path

RUNS = Path(__file__).resolve().parent / "runs"
PRIVATE_ACCOUNT_KEYS = ("email", "organization")


def clip(value, limit=600):
    text = json.dumps(value)
    return text if len(text) <= limit else text[:limit] + "..."


def summarize(name):
    print(f"=== {name}")
    for line in open(RUNS / f"{name}.ndjson", encoding="utf-8"):
        record = json.loads(line)
        msg, t = record["msg"], record["t"]
        kind = msg.get("type")
        if record["dir"] == "note":
            print(f"[{t:>6}] NOTE {clip(msg, 300)}")
            continue
        if kind == "stream_event":
            continue
        prefix = f"[{t:>6}] {'>' if record['dir'] == 'in' else '<'}"
        if kind == "system" and msg.get("subtype") == "init":
            fields = ("apiKeySource", "permissionMode", "model", "cwd", "capabilities")
            print(f"{prefix} INIT {clip({k: msg.get(k) for k in fields})}")
        elif kind == "control_response":
            body = msg["response"]
            payload = body.get("response") or {}
            if "account" in payload:
                account = {k: v for k, v in payload["account"].items()
                           if k not in PRIVATE_ACCOUNT_KEYS}
                print(f"{prefix} INITIALIZE account={clip(account)} keys={sorted(payload)}")
            else:
                print(f"{prefix} CONTROL_RESPONSE {clip(body, 3000)}")
        elif kind == "control_request":
            print(f"{prefix} CONTROL_REQUEST {clip(msg['request'], 1500)}")
        elif kind == "result":
            fields = ("subtype", "is_error", "result", "usage", "modelUsage", "total_cost_usd")
            print(f"{prefix} RESULT {clip({k: msg.get(k) for k in fields}, 2000)}")
        elif kind in ("user", "assistant"):
            print(f"{prefix} {kind.upper()} {clip(msg.get('message', {}).get('content'), 400)}"
                  f" parent={msg.get('parent_tool_use_id')}")
        else:
            print(f"{prefix} {kind}/{msg.get('subtype')} {clip(msg, 800)}")


def main():
    for name in sys.argv[1:]:
        summarize(name)


if __name__ == "__main__":
    main()
