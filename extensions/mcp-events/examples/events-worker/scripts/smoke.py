"""End-to-end proof of the Cloudflare host (A16, A17 in docs/mcp-events.md).

Runs the events-worker under `wrangler dev` and the independent receiver, then
subscribes, publishes, restarts the Worker with its state kept, and publishes
again. Exits non-zero on the first failed check.

Requires: worker-build, npm (wrangler 4), python3.
"""

import json
import os
import pathlib
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request

HERE = pathlib.Path(__file__).resolve().parent.parent
SECRET = "whsec_" + "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY="  # 32 bytes


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


WORKER_PORT = free_port()
RECEIVER_PORT = free_port()
BASE = f"http://127.0.0.1:{WORKER_PORT}"
RECEIVER = f"http://127.0.0.1:{RECEIVER_PORT}"
STATE = tempfile.mkdtemp(prefix="events-worker-state-")
LOG = pathlib.Path(tempfile.mkdtemp(prefix="events-receiver-")) / "requests.jsonl"
LOG.touch()
children = []


def fail(message):
    print(f"FAIL {message}")
    if LOG.exists():
        print(LOG.read_text())
    sys.exit(1)


def check(condition, message):
    if not condition:
        fail(message)
    print(f"ok   {message}")


def spawn(argv, log_path):
    process = subprocess.Popen(
        argv, cwd=HERE, stdout=open(log_path, "w"), stderr=subprocess.STDOUT, start_new_session=True
    )
    children.append(process)
    return process


def stop(process):
    if process.poll() is None:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.wait(timeout=20)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
    children.remove(process)


def start_worker():
    log_path = pathlib.Path(STATE) / f"wrangler-{time.time()}.log"
    process = spawn(
        [
            "npm", "exec", "--yes", "--package=wrangler@4", "--", "wrangler", "dev",
            "--port", str(WORKER_PORT), "--ip", "127.0.0.1", "--persist-to", STATE,
            "--var", "ALLOW_LOCAL_TESTING:true", "--var", "WEBHOOK_TIMEOUT_MS:2000",
        ],
        log_path,
    )
    for _ in range(300):
        if "Ready on" in log_path.read_text():
            return process
        if process.poll() is not None:
            fail("wrangler exited\n" + log_path.read_text())
        time.sleep(1)
    fail("wrangler did not become ready\n" + log_path.read_text())


def post(path, value, principal="p1"):
    request = urllib.request.Request(
        BASE + path,
        data=json.dumps(value).encode(),
        headers={"content-type": "application/json", "x-principal": principal},
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.loads(response.read())


def subscribe(path):
    return post("/mcp", {
        "jsonrpc": "2.0", "id": 1, "method": "events/subscribe",
        "params": {
            "name": "thing.changed",
            "arguments": {"thing": "t1"},
            "delivery": {"mode": "webhook", "url": RECEIVER + path, "secret": SECRET},
            "cursor": None,
        },
    })


def publish(event_id):
    return post("/publish", {
        "eventId": event_id, "name": "thing.changed",
        "attributes": {"thing": "t1"}, "data": {"thing": "t1", "n": event_id},
    })


def records(path=None, kind=None, event_id=None):
    rows = [json.loads(line) for line in LOG.read_text().splitlines() if line.strip()]
    return [
        row for row in rows
        if (path is None or row["path"] == path)
        and (kind is None or row["kind"] == kind)
        and (event_id is None or row["event_id"] == event_id)
    ]


def main():
    spawn([sys.executable, str(HERE / "scripts" / "receiver.py"), str(RECEIVER_PORT), str(LOG), SECRET], LOG.with_suffix(".out"))
    worker = start_worker()

    discover = post("/mcp", {"jsonrpc": "2.0", "id": 0, "method": "server/discover"})
    check(discover["result"]["capabilities"].get("events") == {}, "discovery advertises events")

    for path in ["/ok", "/flaky", "/gone"]:
        response = subscribe(path)
        check(isinstance(response.get("result", {}).get("id"), str), f"subscribe {path}: {response}")
        verifications = records(path, "verification")
        check(len(verifications) == 1 and verifications[0]["valid"], f"{path} got one valid verification")

    redirect = subscribe("/redirect")
    check(redirect.get("error", {}).get("data", {}).get("reason") == "challenge_failed", f"redirect fails challenge_failed: {redirect}")
    check(records("/ok", "verification").__len__() == 1, "redirect was not followed to /ok")
    slow = subscribe("/slow")
    check(slow.get("error", {}).get("data", {}).get("reason") == "timeout", f"slow fails timeout: {slow}")

    published = publish("evt_1")
    check(published.get("matched") == 3, f"evt_1 matched three subscriptions: {published}")
    time.sleep(5)

    ok = records("/ok", "event", "evt_1")
    check(len(ok) == 1 and ok[0]["valid"], "/ok received evt_1 once, signed")
    flaky = records("/flaky", "event", "evt_1")
    check(len(flaky) == 2, f"/flaky received evt_1 twice (got {len(flaky)})")
    check(all(row["valid"] for row in flaky), "both /flaky deliveries are signed")
    check(flaky[0]["webhook_id"] == flaky[1]["webhook_id"] == "evt_1", "retry keeps webhook-id")
    check(flaky[0]["signature"] != flaky[1]["signature"], "retry is signed afresh")
    check(flaky[1]["at"] - flaky[0]["at"] >= 1.0, "retry waited at least 1 second (alarm)")
    check(len(records("/gone", "event", "evt_1")) == 1, "/gone received evt_1")

    stop(worker)
    worker = start_worker()
    published = publish("evt_2")
    check(published.get("matched") == 2, f"after restart evt_2 matched /ok and /flaky only: {published}")
    time.sleep(3)
    check(len(records("/ok", "event", "evt_2")) == 1, "/ok received evt_2 after restart")
    check(len(records("/gone", "event", "evt_2")) == 0, "/gone did not receive evt_2")
    print("PASS events-worker smoke")


try:
    main()
finally:
    for child in list(children):
        stop(child)
