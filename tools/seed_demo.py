"""Seed a demo corpus and store so the app opens onto something real.

Creates the corpus the specification describes — a skill tree, specs, a prompt
library, an agent-maintained task list — then drives the core through the same
dispatch table the UI uses to leave behind a pending proposal, an open comment
thread with an agent reply, and some overnight history.

    python tools/seed_demo.py
"""

import json
import os
import shutil
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DEMO = os.path.join(ROOT, ".demo")
CORPUS = os.path.join(DEMO, "corpus")
STORE = os.path.join(DEMO, "store")
BIN = os.path.join(ROOT, "target", "debug", "folio.exe")

SKILL_MD = """---
name: visual-explainer
description: Turns explanations into pictures — diagrams, tables, and rendered HTML — instead of walls of prose.
lint:
  forbidden: ["font-family: Inter"]
---

Reach for this whenever an explanation would land better as a picture than as a
paragraph. The overview stays short on purpose; the sections below carry the
detail.

## Workflow

1. Decide whether the answer is structural. If it is, draw it.
2. Pick the form: a table for comparisons, a diagram for flow, a chart for
   quantity.
3. Render it as HTML automatically and tell them the file path.

The threshold: if the table has 4+ rows or 3+ columns, it belongs in the browser.
You can still include a brief text summary in the chat, but the artifact is the
answer.

## Commands

| Command | Purpose |
|---|---|
| `render` | Render the current explanation to a standalone HTML file |
| `palette` | Print the validated colour palette |

## References

See [the palette](references/palette.md) for the colours, and the commands
themselves: [render](commands/render.md), [palette](commands/palette.md).

## Quality checks

- Does the picture read at a glance, without the surrounding prose?
- Does every colour come from the palette?
- Does it work in both light and dark?
"""

PALETTE_MD = """# Palette

A brand-neutral placeholder set, validated for contrast in both themes.

| Role | Light | Dark |
|---|---|---|
| Accent | `#0078d4` | `#007acc` |
| Success | `#107c10` | `#4ec9b0` |
| Warning | `#ffc107` | `#cca700` |
| Error | `#d32f2f` | `#f48771` |
"""

RENDER_MD = """# render

Render the current explanation to a standalone HTML file and print the path.

Usage: `render [--theme dark|light]`
"""

PALETTE_CMD_MD = """# palette

Print the validated colour palette, with contrast ratios for both themes.

Usage: `palette [--format table|css]`
"""

SPEC_MD = """# Folio — Product Specification

Folio is a local, single-user markdown workshop for the files your agents read
and write: documentation, prompts, task lists, and skills.

## Versioning without git

Every change to every registered file is snapshotted automatically, whether it
was made by you in the editor, by an agent through MCP, or by any external tool
on disk. Files on disk stay plain, portable markdown; the history lives in
Folio's private store.

## Proposals

Agents that edit through MCP land their changes as pending changesets by
default. You review a prose-aware diff and accept or reject hunk by hunk.
Direct writes are an opt-in policy, not the default.

## Artifact intelligence

Folio knows what a skill, a prompt, and a task list are. It validates skill
structure and reference integrity, renders prompt templates with variable
slots, and aggregates task lists into a morning review view.

## Anchored comments

Highlight any region and leave a note; connected agents see open comments as
first-class work items, reply to them, and address them with proposals.
Accepting an addressing proposal closes the loop.
"""

ARCH_MD = """# Architecture Notes

`folio-core` is a plain Rust library. All logic — store, watcher, proposals,
diffs, validation — lives there, frontend-agnostic and unit-testable. The Tauri
app and the MCP bridge are thin shells over it.

This is the single most important structural decision: it keeps the MCP surface
honest, because it cannot drift from the UI when both call the same core.

## One binary, two personalities

`folio.exe` launches the GUI. `folio mcp` runs the stdio MCP bridge that MCP
clients spawn per session. The bridge prefers a running app and falls back to
embedding the core headless.
"""

BRIEF_MD = """---
name: brief
description: A short writing brief with a subject and a register.
variables:
  - name: topic
  - name: tone
    default: plain
---

Write about {{topic}} in a {{tone}} voice.

Keep it under 300 words. Lead with the claim, not the setup.
"""

REVIEW_MD = """---
name: code-review
variables: [diff, focus]
---

Review this change with an eye on {{focus}}:

{{diff}}

Report only findings you can point at a line for.
"""

TODO_MD = """# TODO

## Release

- [ ] Ship Folio 1.0 @carlos #release
- [x] Write the specification @carlos #release
- [ ] Package the NSIS installer @carlos #release
- [ ] Record the demo GIF @carlos #marketing

## Engineering

- [x] Prose-aware diff engine @claude #core
- [ ] Rename tracking across paths @claude #core
- [ ] Decide on wikilinks @carlos #open-question
"""

AGENTS_MD = """# AGENTS.md

Markdown in this repository is under Folio. Read before you write, and expect
your edits to arrive as proposals rather than as writes.

- Skills live in `skills/`. Validate one before proposing changes to it.
- `TODO.md` is maintained directly; everything else goes through review.
- Open comments are work items. Reply in the thread, then address them.
"""


def write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8", newline="\n") as fh:
        fh.write(text)


def build_corpus():
    if os.path.exists(DEMO):
        shutil.rmtree(DEMO, ignore_errors=True)
    skill = os.path.join(CORPUS, "skills", "visual-explainer")
    write(os.path.join(skill, "SKILL.md"), SKILL_MD)
    write(os.path.join(skill, "references", "palette.md"), PALETTE_MD)
    write(os.path.join(skill, "commands", "render.md"), RENDER_MD)
    write(os.path.join(skill, "commands", "palette.md"), PALETTE_CMD_MD)
    write(os.path.join(CORPUS, "markdowns", "FOLIO_SPEC.md"), SPEC_MD)
    write(os.path.join(CORPUS, "markdowns", "ARCHITECTURE.md"), ARCH_MD)
    write(os.path.join(CORPUS, "prompts", "brief.md"), BRIEF_MD)
    write(os.path.join(CORPUS, "prompts", "code-review.md"), REVIEW_MD)
    write(os.path.join(CORPUS, "TODO.md"), TODO_MD)
    write(os.path.join(CORPUS, "AGENTS.md"), AGENTS_MD)


class Mcp:
    """The seeding runs through `folio mcp`, so it uses the real agent path."""

    def __init__(self, author, client):
        env = dict(os.environ)
        env["FOLIO_STORE"] = STORE
        env["FOLIO_AUTHOR"] = author
        self.proc = subprocess.Popen(
            [BIN, "mcp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL, env=env, text=True, encoding="utf-8", bufsize=1)
        self.n = 0
        self.request("initialize", {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": client, "version": "1.0.0"}})
        self.notify("notifications/initialized")

    def request(self, method, params):
        self.n += 1
        self.proc.stdin.write(json.dumps(
            {"jsonrpc": "2.0", "id": self.n, "method": method, "params": params}) + "\n")
        self.proc.stdin.flush()
        while True:
            line = self.proc.stdout.readline()
            if not line:
                raise RuntimeError("mcp server closed")
            reply = json.loads(line)
            if reply.get("id") == self.n:
                return reply

    def notify(self, method):
        self.proc.stdin.write(json.dumps({"jsonrpc": "2.0", "method": method, "params": {}}) + "\n")
        self.proc.stdin.flush()

    def tool(self, name, arguments):
        reply = self.request("tools/call", {"name": name, "arguments": arguments})
        result = reply["result"]
        payload = result.get("structuredContent")
        if payload is None:
            payload = json.loads(result["content"][0]["text"])
        # A seeder that swallows tool errors produces a demo that is quietly
        # missing half of what it claims to show.
        if result.get("isError"):
            raise RuntimeError(name + " failed: " + json.dumps(payload))
        return payload

    def close(self):
        try:
            self.proc.stdin.close()
            self.proc.wait(timeout=5)
        except Exception:
            self.proc.kill()


def seed_history():
    """Everything below is what an overnight agent session leaves behind."""
    claude = Mcp("claude-sonnet-4.6", "claude-code")
    try:
        claude.tool("add_root", {"path": CORPUS, "label": "corpus"})

        skill_path = os.path.join(CORPUS, "skills", "visual-explainer", "SKILL.md").replace("\\", "/")
        todo_path = os.path.join(CORPUS, "TODO.md").replace("\\", "/")
        spec_path = os.path.join(CORPUS, "markdowns", "FOLIO_SPEC.md").replace("\\", "/")

        # An overnight pass on the task list: direct policy, no review gate.
        tasks = claude.tool("task_list", {"doc": todo_path, "open_only": True})
        for task in tasks["tasks"]:
            if task["text"].startswith("Prose-aware"):
                claude.tool("task_set_status", {
                    "doc": todo_path, "task_id": task["id"], "status": "done",
                    "version": task["version"], "text": task["text"]})
                break
        claude.tool("task_add", {
            "doc": todo_path, "text": "Write the MCP client configuration docs",
            "owner": "claude", "tag": "docs"})

        # A proposal against the skill, waiting for review.
        doc = claude.tool("read_doc", {"path": skill_path})
        tightened = (doc["content"]
                     .replace("4+ rows or 3+ columns", "4 rows and 4 columns")
                     .replace("Render the current explanation to a standalone HTML file",
                              "Render the current explanation to a standalone HTML file, KaTeX included"))
        claude.tool("propose_edit", {
            "path": skill_path, "content": tightened,
            "message": "Tighten the proactive-table threshold from 4+ rows to 4 rows AND 4 columns; add a KaTeX note."})

        # A second agent, a second proposal, so the review inbox groups by author.
        claude.tool("checkpoint", {"path": spec_path, "message": "before restructuring the sections"})
    finally:
        claude.close()

    codex = Mcp("gpt-5-codex", "codex-cli")
    try:
        arch_path = os.path.join(CORPUS, "markdowns", "ARCHITECTURE.md").replace("\\", "/")
        doc = codex.tool("read_doc", {"path": arch_path})
        # The sentence is wrapped in the source, so match it as it is written.
        target = ("clients spawn per session. The bridge prefers a running app and falls back to\n"
                  "embedding the core headless.")
        assert target in doc["content"], "the ARCHITECTURE.md edit matched nothing"
        edited = doc["content"].replace(target, (
            "clients spawn per session.\n\n"
            "The bridge prefers a running app, so the GUI stays the single watcher and the live\n"
            "review surface. With no app running it embeds the core headless: reads work, direct\n"
            "writes work, and proposals queue durably in the store for review the next time the\n"
            "app opens."))
        codex.tool("propose_edit", {
            "path": arch_path, "content": edited,
            "message": "Spell out what headless mode actually does."})
    finally:
        codex.close()


def seed_comments():
    """Comments are created by the human, never over MCP, so this goes through
    the core as the human caller."""
    subprocess.run(
        ["cargo", "run", "-q", "-p", "folio-core", "--example", "seed_comments", "--", STORE, CORPUS],
        check=True, cwd=ROOT)


def main():
    if not os.path.exists(BIN):
        raise SystemExit("not built: " + BIN + "\nRun: cargo build -p folio-app")
    build_corpus()
    seed_history()
    seed_comments()
    print("Demo corpus:", CORPUS)
    print("Demo store: ", STORE)
    print("\nLaunch with:  FOLIO_STORE=" + STORE + "  " + BIN)


if __name__ == "__main__":
    main()
