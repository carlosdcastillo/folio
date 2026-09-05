"""Prove the bridge survives the app closing mid-session.

An agent that is halfway through a task should not fail because you closed the
window. The bridge starts attached to the running app, the app is killed, and
the next tool call must still succeed — against the same store, without one.

    python tools/bridge_failover.py
"""

import json
import os
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, "target", "release", "folio" + (".exe" if os.name == "nt" else ""))
STORE = os.path.join(ROOT, ".demo", "store")


def stop_existing_app():
    command = ["taskkill", "/F", "/IM", "folio.exe"] if os.name == "nt" else ["pkill", "-x", "folio"]
    subprocess.run(command, capture_output=True, check=False)


class Bridge:
    def __init__(self, set_store_env=True):
        env = dict(os.environ)
        if set_store_env:
            env["FOLIO_STORE"] = STORE
        else:
            # How an MCP client configured with a bare `folio mcp` really
            # launches it: no idea which store the app was started with.
            env.pop("FOLIO_STORE", None)
        env["FOLIO_AUTHOR"] = "failover-probe"
        self.proc = subprocess.Popen(
            [BIN, "mcp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, env=env, text=True, encoding="utf-8", bufsize=1)
        self.n = 0
        self.request("initialize", {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "failover-probe", "version": "1"}})
        self.notify("notifications/initialized")

    def request(self, method, params):
        self.n += 1
        self.proc.stdin.write(json.dumps(
            {"jsonrpc": "2.0", "id": self.n, "method": method, "params": params}) + "\n")
        self.proc.stdin.flush()
        while True:
            line = self.proc.stdout.readline()
            if not line:
                raise RuntimeError("bridge closed stdout")
            reply = json.loads(line)
            if reply.get("id") == self.n:
                return reply

    def notify(self, method):
        self.proc.stdin.write(json.dumps({"jsonrpc": "2.0", "method": method, "params": {}}) + "\n")
        self.proc.stdin.flush()

    def tool(self, name, args):
        result = self.request("tools/call", {"name": name, "arguments": args})["result"]
        payload = result.get("structuredContent") or json.loads(result["content"][0]["text"])
        return result.get("isError", False), payload

    def close(self):
        try:
            self.proc.stdin.close()
            self.proc.wait(timeout=5)
        except Exception:
            self.proc.kill()
        # The bridge reports which mode it chose on stderr; that is the only
        # way to tell "bridged" from "headless against an empty store".
        try:
            return self.proc.stderr.read() or ""
        except Exception:
            return ""


def check(condition, label):
    print(("  PASS  " if condition else "  FAIL  ") + label)
    if not condition:
        raise SystemExit(1)


def main():
    if not os.path.exists(BIN):
        raise SystemExit("not built: " + BIN)

    stop_existing_app()
    time.sleep(0.5)

    env = dict(os.environ, FOLIO_STORE=STORE)
    app = subprocess.Popen([BIN], env=env,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(4)

    # The app is on a NON-default store. A bare bridge must still find it:
    # a store-relative rendezvous would send it to an empty default store and
    # every tool call would silently land somewhere the user cannot see.
    bare = Bridge(set_store_env=False)
    try:
        err, roots = bare.tool("list_roots", {})
        paths = [r["path"] for r in roots["roots"]]
        check(not err and any(".demo" in p for p in paths),
              "a bare `folio mcp` finds the app on a non-default store")
    finally:
        note = bare.close()
        check("bridged to the running Folio app" in note,
              "and it bridges rather than going headless")

    bridge = Bridge()
    try:
        err, before = bridge.tool("list_docs", {})
        check(not err and before["docs"], "bridged call works while the app is up")
        count = len(before["docs"])

        stderr_note = "bridged to the running Folio app"
        print("  ...closing the app")
        app.terminate()
        try:
            app.wait(timeout=10)
        except Exception:
            app.kill()
        time.sleep(1.0)

        err, after = bridge.tool("list_docs", {})
        check(not err, "the next call still succeeds after the app is gone")
        check(len(after["docs"]) == count, "and it sees the same corpus (%d docs)" % count)

        err, roots = bridge.tool("list_roots", {})
        check(not err and roots["roots"], "subsequent calls keep working")
        _ = stderr_note
        print("\nBridge failover works.")
    finally:
        bridge.close()
        stop_existing_app()


if __name__ == "__main__":
    sys.exit(main() or 0)
