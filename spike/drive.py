"""Drive `claude -p` over stream-json and record every line in both directions.

Usage: python drive.py <cwd> <scenario>...
Each scenario writes runs/<scenario>.ndjson (both directions, timestamped),
runs/<scenario>.stdout.ndjson (CLI stdout) and runs/<scenario>.stderr.log, all anonymized.
"""

import json
import os
import queue
import shutil
import subprocess
import sys
import threading
import time
import uuid
from pathlib import Path

from anonymize import scrub

RUNS = Path(__file__).resolve().parent / "runs"
STRIPPED_ENV = (
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SSE_PORT",
)
FLAGS = [
    "-p",
    "--input-format", "stream-json",
    "--output-format", "stream-json",
    "--verbose",
    "--include-partial-messages",
    "--permission-prompt-tool", "stdio",
    "--replay-user-messages",
]


class Timeout(Exception):
    pass


class Session:
    def __init__(self, name, cwd):
        RUNS.mkdir(exist_ok=True)
        self.t0 = time.monotonic()
        self.log = open(RUNS / f"{name}.ndjson", "w", encoding="utf-8", newline="\n")
        self.raw = open(RUNS / f"{name}.stdout.ndjson", "wb")
        self.err = open(RUNS / f"{name}.stderr.log", "wb")
        self.inbox = queue.Queue()
        self.next_id = 0
        env = {k: v for k, v in os.environ.items() if k not in STRIPPED_ENV}
        self.session_id = str(uuid.uuid4())
        argv = [shutil.which("claude"), *FLAGS, "--session-id", self.session_id]
        self.note("launch", argv=argv, cwd=str(cwd),
                  stripped_env=[k for k in STRIPPED_ENV if k in os.environ])
        self.proc = subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.PIPE,
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        threading.Thread(target=self._read_stdout, daemon=True).start()
        threading.Thread(target=self._read_stderr, daemon=True).start()

    def _ms(self):
        return round((time.monotonic() - self.t0) * 1000)

    def _record(self, direction, msg):
        self.log.write(scrub(json.dumps({"t": self._ms(), "dir": direction, "msg": msg})) + "\n")
        self.log.flush()

    def note(self, what, **data):
        self._record("note", {"what": what, **data})
        print(f"[{self._ms():>6}] note {what} {json.dumps(data)[:200]}")

    def _read_stdout(self):
        for line in self.proc.stdout:
            self.raw.write(scrub(line.decode("utf-8", "replace")).encode("utf-8"))
            self.raw.flush()
            try:
                msg = json.loads(line)
            except json.JSONDecodeError:
                msg = {"unparsed": line.decode("utf-8", "replace")}
            self._record("out", msg)
            self.inbox.put(msg)
        self.inbox.put(None)

    def _read_stderr(self):
        for line in self.proc.stderr:
            self.err.write(scrub(line.decode("utf-8", "replace")).encode("utf-8"))
            self.err.flush()

    def send(self, msg):
        self._record("in", msg)
        self.proc.stdin.write((json.dumps(msg) + "\n").encode("utf-8"))
        self.proc.stdin.flush()

    def user(self, text):
        print(f"[{self._ms():>6}] user {text}")
        self.send({"type": "user", "message": {"role": "user", "content": text},
                   "parent_tool_use_id": None, "session_id": self.session_id})

    def request(self, subtype, **fields):
        self.next_id += 1
        request_id = f"spike-{self.next_id}"
        self.send({"type": "control_request", "request_id": request_id,
                   "request": {"subtype": subtype, **fields}})
        return request_id

    def pump(self, until, policy=None, timeout=180.0):
        """Handle CLI messages until `until(msg)` is true; returns that message."""
        deadline = time.monotonic() + timeout
        while True:
            left = deadline - time.monotonic()
            if left <= 0:
                raise Timeout()
            try:
                msg = self.inbox.get(timeout=left)
            except queue.Empty:
                raise Timeout()
            if msg is None:
                self.note("stdout closed", code=self.proc.poll())
                raise EOFError()
            self._show(msg)
            if msg.get("type") == "control_request":
                self._answer(msg, policy)
            if until(msg):
                return msg

    def _answer(self, msg, policy):
        req = msg["request"]
        if req.get("subtype") == "can_use_tool" and policy is not None:
            reply = {"subtype": "success", "request_id": msg["request_id"],
                     "response": policy(req)}
        else:
            reply = {"subtype": "error", "request_id": msg["request_id"],
                     "error": f"spike driver has no handler for {req.get('subtype')}"}
        self.send({"type": "control_response", "response": reply})

    def _show(self, msg):
        kind = msg.get("type")
        if kind == "stream_event":
            return
        detail = msg.get("subtype") or ""
        if kind == "control_request":
            r = msg["request"]
            detail = f"{r.get('subtype')} {r.get('tool_name', '')} {json.dumps(r.get('input', ''))[:120]}"
        elif kind == "assistant":
            parts = []
            for block in msg["message"].get("content", []):
                if block.get("type") == "text":
                    parts.append(block["text"][:120])
                elif block.get("type") == "tool_use":
                    parts.append(f"tool_use {block['name']}")
            detail = " | ".join(parts)
        elif kind == "result":
            detail = f"{msg.get('subtype')} {str(msg.get('result', ''))[:120]}"
        print(f"[{self._ms():>6}] {kind} {detail}")

    def control(self, subtype, policy=None, timeout=60.0, **fields):
        request_id = self.request(subtype, **fields)
        return self.pump(lambda m: m.get("type") == "control_response"
                         and m["response"].get("request_id") == request_id,
                         policy, timeout)

    def turn(self, text, policy=None, timeout=180.0):
        self.user(text)
        return self.pump(lambda m: m.get("type") == "result", policy, timeout)

    def drain(self, seconds, policy=None):
        """Keep handling messages for a while, e.g. background task events."""
        try:
            self.pump(lambda m: False, policy, seconds)
        except Timeout:
            pass

    def close(self):
        try:
            self.proc.stdin.close()
            self.proc.wait(timeout=15)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()
        self.note("exit", code=self.proc.returncode)
        self.log.close()
        self.raw.close()
        self.err.close()


def allow(req):
    return {"behavior": "allow", "updatedInput": req["input"]}


def deny_with(message):
    return lambda req: {"behavior": "deny", "message": message}


def answer_last_option(req):
    if req["tool_name"] != "AskUserQuestion":
        return allow(req)
    questions = req["input"]["questions"]
    answers = {q["question"]: q["options"][-1]["label"] for q in questions}
    return {"behavior": "allow", "updatedInput": {**req["input"], "answers": answers}}


def basic(s, cwd):
    s.control("initialize")
    s.turn("Reply with exactly: ok")
    s.control("get_usage", skip_behaviors=True)
    s.turn("Reply with exactly: ok again")


def permissions(s, cwd):
    s.control("initialize")
    s.turn("Use the Bash tool to run exactly: echo spike-allow. Then reply done.", allow)
    s.turn("Use the Write tool to create denied.txt containing x. If refused, reply "
           "refused and do not retry.", deny_with("Refused by the spike driver."))
    s.turn("Call the AskUserQuestion tool once, with the single question 'Pick a color?' "
           "and the options red and blue. Then reply with the answer you got.",
           answer_last_option)


def plan(s, cwd):
    s.control("initialize")
    s.control("set_permission_mode", mode="plan")
    s.turn("Plan creating hello.txt containing hi. Keep the plan to two lines and submit "
           "it with ExitPlanMode. Once approved, create the file, then reply done.", allow)


def is_text_delta(msg):
    event = msg.get("event", {})
    return (msg.get("type") == "stream_event"
            and event.get("type") == "content_block_delta"
            and event.get("delta", {}).get("type") == "text_delta")


def is_tool_use(msg, tool):
    return msg.get("type") == "assistant" and any(
        block.get("type") == "tool_use" and block.get("name") == tool
        for block in msg["message"].get("content", []))


def interrupt(s, cwd):
    s.control("initialize")
    s.user("Write the numbers from 1 to 400, one per line, nothing else.")
    s.pump(is_text_delta, allow)
    s.drain(1, allow)
    s.note("sending interrupt while streaming")
    s.control("interrupt", allow)
    s.pump(lambda m: m.get("type") == "result", allow, 60)
    s.user("Use the Bash tool to run exactly: python -c \"import time; time.sleep(60)\". "
           "Run it in the foreground. Then reply done.")
    s.pump(lambda m: is_tool_use(m, "Bash"), allow)
    s.drain(4, allow)
    s.note("sending interrupt while a tool runs")
    s.control("interrupt", allow)
    s.pump(lambda m: m.get("type") == "result", allow, 60)
    s.turn("Reply with exactly: ok")


def background(s, cwd):
    s.control("initialize")
    s.turn("Use the Bash tool with run_in_background set to true to run exactly: "
           "sleep 8 && echo bg-done. Reply started, without waiting for it.", allow)
    s.drain(40, allow)


def cd(s, cwd):
    target = Path(cwd).parent / "spike-cd-target"
    target.mkdir(exist_ok=True)
    s.control("initialize")
    s.turn(f"/cd {target}", allow)
    s.turn("Use the Bash tool to run exactly: pwd. Reply with its output.", allow)


SCENARIOS = {f.__name__: f for f in (basic, permissions, plan, interrupt, background, cd)}


def main():
    cwd = Path(sys.argv[1])
    cwd.mkdir(parents=True, exist_ok=True)
    for name in sys.argv[2:]:
        print(f"=== {name}")
        s = Session(name, cwd)
        try:
            SCENARIOS[name](s, cwd)
        except (Timeout, EOFError) as e:
            s.note("scenario aborted", error=type(e).__name__)
        finally:
            s.close()


if __name__ == "__main__":
    main()
