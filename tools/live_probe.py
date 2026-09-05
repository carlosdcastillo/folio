"""Make a mutation over MCP and report what the running app should have seen.

    python tools/live_probe.py [--store <dir>] [--no-store-env]

`--no-store-env` deliberately omits FOLIO_STORE, which is how a client
configured with a bare `folio mcp` command actually launches it.
"""

import json
import os
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, "target", "debug", "folio.exe")
DEFAULT_STORE = os.path.join(ROOT, ".demo", "store")


class Bridge:
    def __init__(self, store, set_store_env=True):
        env = dict(os.environ)
        if set_store_env:
            env["FOLIO_STORE"] = store
        else:
            env.pop("FOLIO_STORE", None)
        env["FOLIO_AUTHOR"] = "live-probe-model"
        self.proc = subprocess.Popen(
            [BIN, "mcp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, env=env, text=True, encoding="utf-8", bufsize=1)
        self.n = 0
        self.request("initialize", {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "live-probe", "version": "1"}})
        self.notify("notifications/initialized")

    def request(self, method, params):
        self.n += 1
        self.proc.stdin.write(json.dumps(
            {"jsonrpc": "2.0", "id": self.n, "method": method, "params": params}) + "\n")
        self.proc.stdin.flush()
        while True:
            line = self.proc.stdout.readline()
            if not line:
                raise RuntimeError("bridge closed: " + self.proc.stderr.read())
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
        return self.proc.stderr.read()


def main():
    args = sys.argv[1:]
    store = DEFAULT_STORE
    if "--store" in args:
        store = os.path.abspath(args[args.index("--store") + 1])
    set_env = "--no-store-env" not in args

    print("app store   :", store)
    print("FOLIO_STORE :", "set" if set_env else "NOT SET (as a bare `folio mcp` would run)")
    print("ipc.json    :", "present" if os.path.exists(os.path.join(store, "ipc.json")) else "absent")

    bridge = Bridge(store, set_env)
    try:
        err, roots = bridge.tool("list_roots", {})
        print("roots seen  :", [r["display"] for r in roots["roots"]] or "(none)")

        if not roots["roots"]:
            print("\nThe bridge is looking at a different, empty store.")
            return

        todo = None
        err, docs = bridge.tool("list_docs", {"type": "task_list"})
        if docs["docs"]:
            todo = docs["docs"][0]["path"]

        if todo:
            stamp = time.strftime("%H:%M:%S")
            err, out = bridge.tool("task_add", {
                "doc": todo, "text": "Probe task added at " + stamp,
                "owner": "probe", "tag": "live"})
            print("task_add    :", "ERROR " + json.dumps(out) if err else out.get("outcome"))

        err, doclist = bridge.tool("list_docs", {"type": "doc"})
        target = next((d for d in doclist["docs"] if d["relative"].endswith("AGENTS.md")), None)
        if target:
            err, doc = bridge.tool("read_doc", {"path": target["path"]})
            edited = doc["content"] + "\n- A line proposed by the live probe at " + \
                time.strftime("%H:%M:%S") + ".\n"
            err, out = bridge.tool("propose_edit", {
                "path": target["path"], "content": edited,
                "message": "Live probe: does this reach the app?"})
            print("propose_edit:", "ERROR " + json.dumps(out) if err else out.get("outcome"))
    finally:
        stderr = bridge.close()
        print("bridge mode :", stderr.strip() or "(no stderr)")


if __name__ == "__main__":
    main()
