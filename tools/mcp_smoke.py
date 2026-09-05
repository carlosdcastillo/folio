"""Drive `folio mcp` over real stdio MCP and walk the agent's whole loop.

The unit and integration tests exercise the core through `dispatch`. This one
exercises the thing an MCP client actually spawns: the binary, the JSON-RPC
framing, the tool schemas, and the round trip back.

    python tools/mcp_smoke.py [path-to-folio.exe]
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DEFAULT_BIN = os.path.join(ROOT, "target", "debug", "folio.exe")

SKILL_MD = """---
name: visual-explainer
description: Renders explanations as pictures.
---

Short overview that routes the reader deeper.

## Workflow

The threshold: if the table has 4+ rows or 3+ columns, it belongs in the browser.

## Commands

| Command | Purpose |
|---|---|
| `render` | Render it |

See [palette](references/palette.md) and [render](commands/render.md).

## Quality checks

- Does it read at a glance?
"""

TODO_MD = """# TODO

- [ ] Ship Folio 1.0 @carlos #release
- [x] Write the spec @carlos
"""


class Client:
    """A minimal MCP client: line-delimited JSON-RPC 2.0 over stdio."""

    def __init__(self, binary, store, author="claude-sonnet-4.6"):
        env = dict(os.environ)
        env["FOLIO_STORE"] = store
        env["FOLIO_AUTHOR"] = author
        self.proc = subprocess.Popen(
            [binary, "mcp"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
            text=True,
            encoding="utf-8",
            bufsize=1,
        )
        self.next_id = 0

    def send(self, method, params=None, notify=False):
        message = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            message["params"] = params
        if not notify:
            self.next_id += 1
            message["id"] = self.next_id
        self.proc.stdin.write(json.dumps(message) + "\n")
        self.proc.stdin.flush()
        if notify:
            return None
        while True:
            line = self.proc.stdout.readline()
            if not line:
                raise RuntimeError("server closed stdout; stderr:\n" + self.proc.stderr.read())
            line = line.strip()
            if not line:
                continue
            reply = json.loads(line)
            # Skip anything that is not the response to this request.
            if reply.get("id") == message["id"]:
                return reply

    def call_tool(self, name, arguments=None):
        reply = self.send("tools/call", {"name": name, "arguments": arguments or {}})
        if "error" in reply:
            raise RuntimeError(name + " protocol error: " + json.dumps(reply["error"]))
        result = reply["result"]
        payload = result.get("structuredContent")
        if payload is None:
            payload = json.loads(result["content"][0]["text"])
        return result.get("isError", False), payload

    def close(self):
        try:
            self.proc.stdin.close()
        except Exception:
            pass
        try:
            self.proc.wait(timeout=5)
        except Exception:
            self.proc.kill()


def check(condition, label):
    print(("  PASS  " if condition else "  FAIL  ") + label)
    if not condition:
        raise SystemExit(1)


def main():
    binary = os.path.abspath(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_BIN
    if not os.path.exists(binary):
        raise SystemExit("not built: " + binary + "\nRun: cargo build -p folio-app")

    workdir = tempfile.mkdtemp(prefix="folio-mcp-")
    corpus = os.path.join(workdir, "corpus")
    skill = os.path.join(corpus, "skills", "visual-explainer")
    os.makedirs(os.path.join(skill, "commands"))
    os.makedirs(os.path.join(skill, "references"))
    with open(os.path.join(skill, "SKILL.md"), "w", encoding="utf-8") as fh:
        fh.write(SKILL_MD)
    for name, where in [("render.md", "commands"), ("palette.md", "references")]:
        with open(os.path.join(skill, where, name), "w", encoding="utf-8") as fh:
            fh.write("# " + name + "\n")
    with open(os.path.join(corpus, "TODO.md"), "w", encoding="utf-8") as fh:
        fh.write(TODO_MD)

    client = Client(binary, os.path.join(workdir, "store"))
    try:
        print("initialize")
        reply = client.send("initialize", {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "claude-code", "version": "1.0.0"},
        })
        check("result" in reply, "handshake succeeded")
        info = reply["result"]
        check(info["serverInfo"]["name"] == "folio", "server identifies as folio")
        check("proposal" in info.get("instructions", "").lower(), "instructions explain the proposal gate")
        client.send("notifications/initialized", {}, notify=True)

        print("tools/list")
        tools = client.send("tools/list", {})["result"]["tools"]
        names = sorted(t["name"] for t in tools)
        print("        " + ", ".join(names))
        expected = {
            "add_root", "checkpoint", "diff_versions", "list_comments", "list_docs",
            "list_proposals", "list_roots", "list_versions", "propose_edit", "read_doc",
            "read_version", "render_prompt", "reply_comment", "search_docs",
            "task_add", "task_list", "task_set_status", "validate_doc",
        }
        check(set(names) == expected, "the tool surface is exactly the specified one")
        check(all("inputSchema" in t and t["inputSchema"]["type"] == "object" for t in tools),
              "every tool declares an object input schema")
        check(not any(n in names for n in ("accept_proposal", "reject_proposal", "resolve_comment")),
              "no tool lets an agent decide a proposal or resolve a thread")

        print("add_root")
        err, payload = client.call_tool("add_root", {"path": corpus, "label": "corpus"})
        check(not err, "root registered")
        check(payload["indexed"] >= 4, "files indexed: %s" % payload["indexed"])

        print("list_docs / read_doc")
        err, docs = client.call_tool("list_docs", {})
        types = {d["relative"]: d["type"] for d in docs["docs"]}
        check(types.get("skills/visual-explainer/SKILL.md") == "skill", "SKILL.md typed as a skill")
        check(types.get("TODO.md") == "task_list", "TODO.md typed as a task list")

        skill_path = os.path.join(corpus, "skills", "visual-explainer", "SKILL.md")
        err, doc = client.call_tool("read_doc", {"path": skill_path})
        check(doc["frontmatter"]["name"] == "visual-explainer", "frontmatter parsed")
        check(doc["policy"] == "propose", "a skill is gated behind review")

        print("search_docs")
        err, hits = client.call_tool("search_docs", {"query": "threshold"})
        check(hits["count"] == 1 and hits["matches"][0]["line"] > 1, "search returns located matches")

        print("validate_doc")
        err, report = client.call_tool("validate_doc", {"path": skill_path})
        check(report["ok"] and report["type"] == "skill", "the skill validates clean")

        print("propose_edit")
        tightened = doc["content"].replace("4+ rows or 3+ columns", "4 rows and 4 columns")
        err, outcome = client.call_tool("propose_edit", {
            "path": skill_path,
            "content": tightened,
            "message": "Tighten the proactive-table threshold from 4+ rows to 4 rows AND 4 columns.",
        })
        check(not err and outcome["outcome"] == "proposed", "the edit became a proposal, not a write")
        with open(skill_path, encoding="utf-8") as fh:
            check("4+ rows or 3+ columns" in fh.read(), "the file on disk is untouched")

        err, listed = client.call_tool("list_proposals", {"status": "pending", "mine_only": True})
        check(listed["count"] == 1, "the proposal is queued for review")
        check(listed["proposals"][0]["author"] == "claude-sonnet-4.6", "authored by the model, not the client")

        print("list_versions / diff_versions")
        err, versions = client.call_tool("list_versions", {"path": skill_path})
        check(versions["versions"][0]["source"] == "external", "indexing recorded a version")

        print("checkpoint")
        err, cp = client.call_tool("checkpoint", {"path": skill_path, "message": "before restructuring commands"})
        check(cp["snapshot"]["source"] == "checkpoint", "agents can mark milestones too")

        print("task_list / task_set_status")
        todo = os.path.join(corpus, "TODO.md")
        err, tasks = client.call_tool("task_list", {"doc": todo, "open_only": True})
        check(tasks["count"] == 1, "one open task")
        task = tasks["tasks"][0]

        err, applied = client.call_tool("task_set_status", {
            "doc": todo, "task_id": task["id"], "status": "done",
            "version": task["version"], "text": task["text"],
        })
        check(not err and applied["outcome"] == "applied", "task lists write through, no review gate")
        with open(todo, encoding="utf-8") as fh:
            check("- [x] Ship Folio 1.0 @carlos #release" in fh.read(), "the checkbox flipped on disk")

        print("task_set_status (stale)")
        err, stale = client.call_tool("task_set_status", {
            "doc": todo, "task_id": task["id"], "status": "open", "version": task["version"],
        })
        check(err, "a stale write is refused")
        check(stale["code"] == "stale" and "tasks" in stale.get("data", {}),
              "and it comes back with the current list attached")

        print("task_add")
        err, added = client.call_tool("task_add", {
            "doc": todo, "text": "Package the installer", "owner": "carlos", "tag": "release",
        })
        check(not err, "task added")
        with open(todo, encoding="utf-8") as fh:
            check("- [ ] Package the installer @carlos #release" in fh.read(), "grammar preserved on write")

        print("list_comments")
        err, comments = client.call_tool("list_comments", {"status": "open"})
        check(comments["count"] == 0, "no open threads in a fresh corpus")

        print("sandboxing")
        err, escape = client.call_tool("read_doc", {"path": os.path.join(corpus, "..", "outside.md")})
        check(err and escape["code"] in ("outside_root", "not_found"), "a path escape is rejected")

        print("\nAll MCP checks passed.")
    finally:
        client.close()
        shutil.rmtree(workdir, ignore_errors=True)


if __name__ == "__main__":
    main()
