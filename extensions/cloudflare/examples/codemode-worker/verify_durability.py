"""Verify Code Mode heap recovery through a real local Worker and MCP transport.

Run worker-build --release first. This probe owns its Wrangler process group,
restarts it twice, retains captures, and exits nonzero on an assertion failure.
"""

import argparse
import json
import os
import pathlib
import signal
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parent
PROGRAM = """async () => {
  const stamp = Math.random().toString();
  const remember = () => stamp;
  const held = 'z'.repeat(2100000);
  const bytes = new Uint8Array([7, 8, 255]);
  const counts = new Map([['total', 0]]);
  const receipts = [];
  const sum = await math.sum({left: 40, right: 3});
  for (let index = 0; index < 2; index++) {
    receipts.push(await math.save({value: stamp + ':' + index}));
    counts.set('total', counts.get('total') + index + 1);
  }
  console.log('prelude', stamp);
  let retainedChecksum = 0;
  for (let index = 0; index < held.length; index++) retainedChecksum += held.charCodeAt(index);
  return {stamp, remembered: remember(), total: counts.get('total'), bytes,
          retainedLength: held.length, retainedChecksum, sum: sum.sum, receipts,
          executionId: codemode.executionId, large: 'q'.repeat(1000100)};
}"""


class Worker:
    def __init__(self, evidence):
        self.evidence = evidence
        self.process = None
        self.log = None
        self.starts = 0
        self.rpc_id = 0
        with socket.socket() as port:
            port.bind(("127.0.0.1", 0))
            self.port = port.getsockname()[1]
        self.url = f"http://127.0.0.1:{self.port}"
        config = json.loads((ROOT / "wrangler.jsonc").read_text())
        config.pop("build")
        config["main"] = str(ROOT / "build/index.js")
        self.config = evidence / "wrangler.jsonc"
        self.config.write_text(json.dumps(config, indent=2))

    def start(self):
        self.starts += 1
        log_path = self.evidence / f"worker-{self.starts}.log"
        self.log = log_path.open("w")
        self.process = subprocess.Popen(
            ["wrangler", "dev", "--config", str(self.config), "--port", str(self.port),
             "--ip", "127.0.0.1", "--persist-to", str(self.evidence / "storage")],
            cwd=ROOT, stdout=self.log, stderr=subprocess.STDOUT, start_new_session=True,
        )
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                raise AssertionError(log_path.read_text()[-4000:])
            if "Ready on" in log_path.read_text():
                initialized = self.rpc("initialize", {
                    "protocolVersion": "2025-11-25", "capabilities": {},
                    "clientInfo": {"name": "incurs-durability-probe", "version": "1"},
                })
                assert initialized["serverInfo"]["name"] == "incurs-codemode"
                return
            time.sleep(0.05)
        raise AssertionError("Worker readiness deadline exceeded: " + log_path.read_text()[-4000:])

    def stop(self, abrupt=False):
        if self.process is not None:
            if self.process.poll() is None:
                os.killpg(self.process.pid, signal.SIGKILL if abrupt else signal.SIGTERM)
                try:
                    self.process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(self.process.pid, signal.SIGKILL)
                    self.process.wait(timeout=5)
            self.process = None
        if self.log:
            self.log.close()
            self.log = None

    def rpc(self, method, params):
        self.rpc_id += 1
        body = json.dumps({"jsonrpc": "2.0", "id": self.rpc_id, "method": method, "params": params}).encode()
        request = urllib.request.Request(self.url + "/mcp", data=body, headers={
            "Content-Type": "application/json", "Accept": "application/json, text/event-stream",
            "MCP-Protocol-Version": "2025-11-25",
        })
        with urllib.request.urlopen(request, timeout=30) as response:
            envelope = json.load(response)
        assert "error" not in envelope, envelope
        return envelope["result"]

    def call(self, name, arguments, error=False):
        result = self.rpc("tools/call", {"name": name, "arguments": arguments})
        if error:
            assert result.get("isError"), result
            return result
        assert not result.get("isError"), result
        return result["structuredContent"]

    def state(self, execution, expected):
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            state = self.call("codemode_execution", {"id": execution})
            if state["status"] == expected:
                return state
            assert state["status"] == "running", {"status": state["status"], "error": state["error"]}
            time.sleep(0.02)
        raise AssertionError(f"Execution {execution} did not become {expected}")

    def receipts(self):
        with urllib.request.urlopen(self.url + "/receipts", timeout=5) as response:
            return json.load(response)


def verify(worker):
    worker.start()
    listed = worker.rpc("tools/list", {})
    assert [tool["name"] for tool in listed["tools"]] == [
        "codemode_search", "codemode_execute", "codemode_execution", "codemode_decide", "codemode_cancel",
    ]
    started = worker.call("codemode_execute", {"code": PROGRAM})
    execution = started["id"]
    paused = worker.state(execution, "paused")
    assert paused["log"][-1]["seq"] == 1
    stamp = paused["log"][-1]["arguments"]["value"].removesuffix(":0")
    assert worker.receipts() == []
    worker.stop(abrupt=True)
    worker.start()
    worker.state(execution, "paused")
    worker.call("codemode_execution", {"id": execution, "resume": True}, error=True)
    worker.call("codemode_decide", {"id": execution, "seq": 1, "decision": "approve"})
    paused = worker.state(execution, "paused")
    assert paused["log"][-1]["seq"] == 2
    assert worker.receipts() == [{"id": 1, "value": stamp + ":0"}]
    worker.stop(abrupt=True)
    worker.start()
    worker.call("codemode_decide", {"id": execution, "seq": 2, "decision": "approve"})
    completed = worker.state(execution, "completed")
    artifact = completed["result"]["$artifact"]
    result = worker.call("codemode_execution", {"id": execution, "artifact_id": artifact["id"]})
    assert result["stamp"] == result["remembered"] == stamp
    assert result["sum"] == 43
    assert result["total"] == 3
    assert result["bytes"] == {"__codemode_type": "binary", "value": "Bwj/"}
    assert result["retainedLength"] == 2100000
    assert result["retainedChecksum"] == 256200000
    assert result["executionId"] == execution
    assert result["large"] == "q" * 1000100
    assert completed["logs"] == ["prelude " + stamp]
    expected = [{"id": 1, "value": stamp + ":0"}, {"id": 2, "value": stamp + ":1"}]
    assert worker.receipts() == expected
    worker.call("codemode_decide", {"id": execution, "seq": 2, "decision": "approve"})
    assert worker.receipts() == expected
    runaway = worker.call("codemode_execute", {"code": "async () => { while (true) {} }"})
    failed = worker.state(runaway["id"], "error")
    assert "CODE_EXECUTION_TIMEOUT" in failed["error"], failed["error"]
    return {"verdict": "pass", "execution": execution, "worker_starts": worker.starts,
            "heap_bytes_retained": 2100000, "artifact_bytes": artifact["bytes"],
            "save_receipts": expected, "runaway_error": failed["error"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--evidence-dir", type=pathlib.Path)
    args = parser.parse_args()
    evidence = args.evidence_dir or pathlib.Path(tempfile.mkdtemp(prefix="incurs-worker-durability-"))
    evidence.mkdir(parents=True, exist_ok=True)
    worker = Worker(evidence.resolve())
    try:
        report = verify(worker)
    except Exception as error:
        report = {"verdict": "fail", "error": str(error), "evidence": str(evidence)}
        raise
    finally:
        worker.stop()
        if "report" in locals():
            (evidence / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({**report, "evidence": str(evidence)}))


if __name__ == "__main__":
    main()
