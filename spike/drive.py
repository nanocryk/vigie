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
    def __init__(self, name, cwd, extra_flags=()):
        RUNS.mkdir(exist_ok=True)
        self.t0 = time.monotonic()
        self.seen = []
        self.log = open(RUNS / f"{name}.ndjson", "w", encoding="utf-8", newline="\n")
        self.raw = open(RUNS / f"{name}.stdout.ndjson", "wb")
        self.err = open(RUNS / f"{name}.stderr.log", "wb")
        self.inbox = queue.Queue()
        self.next_id = 0
        env = {k: v for k, v in os.environ.items() if k not in STRIPPED_ENV}
        self.session_id = str(uuid.uuid4())
        argv = [shutil.which("claude"), *FLAGS, *extra_flags, "--session-id", self.session_id]
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

    def user(self, text, parent_tool_use_id=None, priority=None):
        print(f"[{self._ms():>6}] user {text}")
        msg = {"type": "user", "message": {"role": "user", "content": text},
               "parent_tool_use_id": parent_tool_use_id, "session_id": self.session_id}
        if priority is not None:
            msg["priority"] = priority
        self.send(msg)

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
            self.seen.append(msg)
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


def midturn(s, cwd):
    s.control("initialize")
    for priority, marker in ((None, "plain"), ("next", "next")):
        s.user("Use the Bash tool three times, one call per message, each running exactly: "
               "python -c \"import time; time.sleep(6)\". Run them in the foreground. "
               "Then reply done.")
        s.pump(lambda m: is_tool_use(m, "Bash"), allow)
        s.drain(1, allow)
        s.note("sending a message mid-turn", priority=priority)
        s.user(f"If you read this before your final reply, include the word "
               f"marker-{marker} in it.", priority=priority)
        s.pump(lambda m: m.get("type") == "result", allow, 120)
        s.drain(20, allow)


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


def context(s, cwd):
    s.control("initialize")
    s.note("context before any turn")
    s.control("get_context_usage")
    s.turn("Reply with exactly: ok")
    s.note("context after a turn")
    s.control("get_context_usage")
    s.control("get_context_usage", detail="summary")
    s.user("Use the Bash tool to run exactly: python -c \"import time; time.sleep(8)\". "
           "Run it in the foreground. Then reply done.")
    s.pump(lambda m: is_tool_use(m, "Bash"), allow)
    s.note("context while a tool runs")
    s.control("get_context_usage", allow)
    s.pump(lambda m: m.get("type") == "result", allow, 60)
    s.note("context after the tool turn")
    s.control("get_context_usage")


def grants(s, cwd):
    outside = Path(cwd).parent / "outside"
    for rel in ("a/b/one.txt", "a/c/two.txt", "other.txt"):
        (outside / rel).parent.mkdir(parents=True, exist_ok=True)
        (outside / rel).write_text(rel, encoding="utf-8")
    for name in ("edit1.txt", "edit2.txt"):
        (Path(cwd) / name).unlink(missing_ok=True)

    def asks_logged(policy):
        def logged(req):
            s.note("ask", tool=req["tool_name"], input=req["input"],
                   suggestions=req.get("permission_suggestions"))
            return policy(req)
        return logged

    def allow_parent(req):
        suggestion = req["permission_suggestions"][0]
        rule = suggestion["rules"][0]
        folder = rule["ruleContent"].removesuffix("/**").rsplit("/", 1)[0]
        parent = {**suggestion, "rules": [{**rule, "ruleContent": folder + "/**"}]}
        return {"behavior": "allow", "updatedInput": req["input"],
                "updatedPermissions": [parent]}

    def allow_with_suggestions(req):
        return {"behavior": "allow", "updatedInput": req["input"],
                "updatedPermissions": req.get("permission_suggestions") or []}

    def read(rel):
        return (f"Use the Read tool on exactly {outside / rel}. Reply with its content, "
                "or refused if refused, and do not retry.")

    s.control("initialize")
    s.note("read under the suggested folder, granting its parent")
    s.turn(read("a/b/one.txt"), asks_logged(allow_parent))
    s.note("read a sibling under the granted parent, expecting no ask")
    s.turn(read("a/c/two.txt"), asks_logged(deny_with("Asked again.")))
    s.note("read outside the grant, expecting an ask")
    s.turn(read("other.txt"), asks_logged(deny_with("Refused by the spike driver.")))
    s.note("write, allowing with the accept-edits suggestion")
    s.turn("Use the Write tool to create edit1.txt containing x. Then reply done.",
           asks_logged(allow_with_suggestions))
    s.note("second write, expecting no ask")
    s.turn("Use the Write tool to create edit2.txt containing y. If refused, reply "
           "refused and do not retry.", asks_logged(deny_with("Asked again.")))
    refused = asks_logged(deny_with("Refused by the spike driver."))
    probes = {
        "Write": f"create {outside / 'w' / 'new.txt'} containing z",
        "Glob": f"find *.txt files under {outside}",
        "Grep": f"search for the word other under {outside}",
        "Bash": f"run exactly: cat \"{outside / 'other.txt'}\"",
    }
    for tool, action in probes.items():
        s.note(f"{tool} outside the folder, recording its suggestions")
        s.turn(f"Use the {tool} tool to {action}. Reply with the result, or refused if "
               "refused, and do not retry or use another tool.", refused)


def agent_task(s, description):
    """The `task_started` of the background agent named `description`."""
    for msg in s.seen:
        if (msg.get("type") == "system" and msg.get("subtype") == "task_started"
                and msg.get("description") == description):
            return msg
    return None


def spawn_agent(s, description, prompt):
    s.turn(f"Use the Agent tool once, with run_in_background set to true, subagent_type "
           f"general-purpose, description '{description}', and this prompt: '{prompt}' "
           "Do not wait for it; reply started.", allow)
    started = agent_task(s, description)
    s.note("agent started", description=description,
           task=None if started is None else {k: started.get(k) for k in (
               "task_id", "tool_use_id", "task_type", "subagent_type", "is_backgrounded",
               "spawn_depth")})
    return started


def agents(s, cwd):
    s.control("initialize", perTaskStopAffordance=True)
    sleeper = spawn_agent(
        s, "sleeper",
        "Use the Bash tool to run exactly: python -c \"import time; time.sleep(20)\". "
        "Then reply with the single word DONE, unless a message told you another word; "
        "then reply with that word instead.")
    if sleeper is not None:
        s.note("message to the subagent through parent_tool_use_id")
        s.user("Change of plan: when you finish, reply with the single word PINEAPPLE.",
               parent_tool_use_id=sleeper.get("tool_use_id"))
    s.drain(60, allow)
    stopper = spawn_agent(
        s, "stopper",
        "Use the Bash tool to run exactly: python -c \"import time; time.sleep(60)\". "
        "Then reply DONE.")
    s.drain(5, allow)
    if stopper is not None:
        s.note("stop_task on the second agent")
        s.control("stop_task", allow, task_id=stopper["task_id"])
    s.drain(20, allow)


def is_task_started(msg):
    return msg.get("type") == "system" and msg.get("subtype") == "task_started"


def stoptool(s, cwd):
    s.control("initialize")
    s.user("Use the Bash tool to run exactly: python -c \"import time; time.sleep(60)\". "
           "Run it in the foreground. Then reply with the single word AFTER and what the "
           "tool returned.")
    started = s.pump(is_task_started, allow)
    s.drain(3, allow)
    s.note("stop_task on the foreground Bash", task_id=started["task_id"])
    s.control("stop_task", allow, task_id=started["task_id"])
    s.pump(lambda m: m.get("type") == "result", allow, 90)
    s.drain(5, allow)
    s.user("Use the Bash tool with run_in_background set to true to run exactly: "
           "python -c \"import time; time.sleep(60)\". Reply started, without waiting for it.")
    started = s.pump(is_task_started, allow)
    s.pump(lambda m: m.get("type") == "result", allow, 60)
    s.note("stop_task on the background Bash", task_id=started["task_id"])
    s.control("stop_task", allow, task_id=started["task_id"])
    s.drain(30, allow)
    s.user("Use the Agent tool once, in the foreground (run_in_background false), subagent_type "
           "general-purpose, description 'sleeper', and this prompt: 'Use the Bash tool to run "
           "exactly: python -c \"import time; time.sleep(60)\" in the foreground. Then reply "
           "with the single word AFTER and what the tool returned.' Then reply with what the "
           "agent returned.")
    started = s.pump(lambda m: is_task_started(m) and m.get("task_type") == "local_bash", allow)
    s.drain(3, allow)
    s.note("stop_task on the agent's foreground Bash", task_id=started["task_id"],
           owned_by_subagent=started.get("owned_by_subagent"))
    s.control("stop_task", allow, task_id=started["task_id"])
    s.pump(lambda m: m.get("type") == "result", allow, 120)


def posix_rule_path(path):
    """`C:\\a\\b` as `//c/a/b`, the form the CLI's own rule suggestions use."""
    drive, rest = str(path).split(":", 1)
    return f"//{drive.lower()}{rest.replace(chr(92), '/')}"


def filerules(s, cwd):
    plain = Path(cwd).parent / "outside-rules" / "note.md"
    sensitive = Path.home() / ".claude" / "plans" / "vigie2-spike-rule.md"

    def rule(tool, content):
        return {"type": "addRules", "rules": [{"toolName": tool, "ruleContent": content}],
                "behavior": "allow", "destination": "session"}

    def logged(label, policy):
        def answer(req):
            s.note("ask", label=label, tool=req["tool_name"], input=req["input"],
                   suppress_always_allow_rule=req.get("suppress_always_allow_rule"),
                   decision_reason=req.get("decision_reason"),
                   decision_reason_type=req.get("decision_reason_type"),
                   suggestions=req.get("permission_suggestions"))
            return policy(req)
        return answer

    def allow_rule(tool, content):
        return lambda req: {"behavior": "allow", "updatedInput": req["input"],
                            "updatedPermissions": [rule(tool, content)]}

    def edit(path, old, new):
        return (f"Use the Edit tool on exactly {path} to replace '{old}' with '{new}'. "
                "Reply done, or refused if refused, and do not retry or use another tool.")

    def read(path):
        return (f"Use the Read tool on exactly {path}. Reply with its content, or refused if "
                "refused, and do not retry or use another tool.")

    s.control("initialize")
    try:
        for name, path in (("plain", plain), ("sensitive", sensitive)):
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("one\n", encoding="utf-8")
            s.note(f"{name}: first read, allowing with a Read rule in //c/ form")
            s.turn(read(path), logged(f"{name} read 1", allow_rule("Read", posix_rule_path(path))))
            s.note(f"{name}: second read, an ask means the rule did not hold")
            s.turn(read(path), logged(f"{name} read 2", deny_with("Asked again.")))
            s.note(f"{name}: first edit, allowing with an Edit rule in //c/ form")
            s.turn(edit(path, "one", "two"),
                   logged(f"{name} edit 1", allow_rule("Edit", posix_rule_path(path))))
            s.note(f"{name}: second edit, an ask means the //c/ rule did not hold; "
                   "allowing with a Windows-path rule")
            s.turn(edit(path, "two", "three"),
                   logged(f"{name} edit 2", allow_rule("Edit", str(path))))
            s.note(f"{name}: third edit, an ask means no rule form held")
            s.turn(edit(path, "three", "four"),
                   logged(f"{name} edit 3", deny_with("Asked again.")))
    finally:
        sensitive.unlink(missing_ok=True)


SCENARIOS = {f.__name__: f for f in (basic, permissions, plan, interrupt, background, cd,
                                     context, grants, agents, filerules, midturn, stoptool)}
FLAGS_BY_SCENARIO = {"agents": ("--forward-subagent-text",),
                     "stoptool": ("--forward-subagent-text",)}


def main():
    cwd = Path(sys.argv[1])
    cwd.mkdir(parents=True, exist_ok=True)
    for name in sys.argv[2:]:
        print(f"=== {name}")
        s = Session(name, cwd, FLAGS_BY_SCENARIO.get(name, ()))
        try:
            SCENARIOS[name](s, cwd)
        except (Timeout, EOFError) as e:
            s.note("scenario aborted", error=type(e).__name__)
        finally:
            s.close()


if __name__ == "__main__":
    main()
