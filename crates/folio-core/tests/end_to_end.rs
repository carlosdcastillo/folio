//! End-to-end tests written directly against the specification's success
//! criteria, driven through the same `dispatch` entry point the UI and the MCP
//! bridge use. If these pass, the two moments of truth in the spec — an
//! agent's edit arriving as a reviewable proposal, and a comment closing the
//! loop end to end — actually work.

use folio_core::api::{Caller, Folio};
use serde_json::{json, Value};
use std::sync::Arc;

struct Bed {
    dir: tempfile::TempDir,
    folio: Arc<Folio>,
}

fn agent() -> Caller {
    Caller::agent("claude-sonnet-4.6", "claude-code")
}

fn you() -> Caller {
    Caller::human()
}

impl Bed {
    /// A corpus that looks like the one the spec describes: a skill tree, a
    /// spec document, a prompt library, and an agent-maintained task list.
    fn new() -> Bed {
        let dir = tempfile::tempdir().unwrap();
        let corpus = dir.path().join("corpus");

        let skill = corpus.join("skills").join("visual-explainer");
        std::fs::create_dir_all(skill.join("commands")).unwrap();
        std::fs::create_dir_all(skill.join("references")).unwrap();
        std::fs::write(skill.join("commands").join("render.md"), "# render\n").unwrap();
        std::fs::write(skill.join("references").join("palette.md"), "# palette\n").unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            SKILL_MD,
        )
        .unwrap();

        std::fs::create_dir_all(corpus.join("markdowns")).unwrap();
        std::fs::write(corpus.join("markdowns").join("DESIGN.md"), DESIGN_MD).unwrap();

        std::fs::create_dir_all(corpus.join("prompts")).unwrap();
        std::fs::write(corpus.join("prompts").join("brief.md"), BRIEF_MD).unwrap();

        std::fs::write(corpus.join("TODO.md"), TODO_MD).unwrap();

        let folio = Folio::open(&dir.path().join("store")).unwrap();
        folio
            .dispatch(
                &you(),
                "add_root",
                &json!({ "path": corpus.to_string_lossy(), "label": "corpus" }),
            )
            .unwrap();

        Bed { dir, folio }
    }

    fn path(&self, relative: &str) -> String {
        self.dir
            .path()
            .join("corpus")
            .join(relative)
            .to_string_lossy()
            .to_string()
    }

    fn call(&self, caller: &Caller, op: &str, params: Value) -> Value {
        self.folio
            .dispatch(caller, op, &params)
            .unwrap_or_else(|e| panic!("{op} failed: {e}"))
    }

    fn try_call(&self, caller: &Caller, op: &str, params: Value) -> folio_core::Result<Value> {
        self.folio.dispatch(caller, op, &params)
    }

    fn read(&self, relative: &str) -> String {
        std::fs::read_to_string(self.dir.path().join("corpus").join(relative)).unwrap()
    }
}

const SKILL_MD: &str = "---\nname: visual-explainer\ndescription: Renders explanations as pictures.\n---\n\n\
Short overview.\n\n\
## Workflow\n\n\
render it as HTML automatically and tell them the file path.\n\
The threshold: if the table has 4+ rows or 3+ columns, it belongs in the browser.\n\
You can still include a brief text summary in the chat,\n\n\
## Commands\n\n\
| Command | Purpose |\n|---|---|\n| `render` | Render it |\n\n\
See [palette](references/palette.md) and [render](commands/render.md).\n\n\
## Quality checks\n\n\
- Does it read at a glance?\n";

const DESIGN_MD: &str = "# Design\n\n\
The first paragraph of the design document.\n\n\
The second paragraph, which will be rewritten.\n\n\
The third paragraph, left alone.\n";

const BRIEF_MD: &str = "---\nname: brief\nvariables: [topic]\n---\n\n\
Write about {{topic}} in a {{tone}} voice.\n";

const TODO_MD: &str = "# TODO\n\n\
- [ ] Ship Folio 1.0 @carlos #release\n\
- [x] Write the spec @carlos\n\
- [ ] Package the installer @carlos #release\n";

// ---------------------------------------------------------------------------
// Corpus, typing, sandboxing
// ---------------------------------------------------------------------------

#[test]
fn the_corpus_indexes_and_types_by_structure() {
    let bed = Bed::new();
    let docs = bed.call(&agent(), "list_docs", json!({}));
    let docs = docs["docs"].as_array().unwrap();

    let by_type = |want: &str| -> Vec<String> {
        docs.iter()
            .filter(|d| d["type"] == want)
            .map(|d| d["relative"].as_str().unwrap().to_string())
            .collect()
    };

    assert_eq!(by_type("skill"), vec!["skills/visual-explainer/SKILL.md"]);
    assert_eq!(by_type("prompt"), vec!["prompts/brief.md"]);
    assert_eq!(by_type("task_list"), vec!["TODO.md"]);
    assert!(by_type("doc").contains(&"markdowns/DESIGN.md".to_string()));
    // The skill's own files are tracked too, as plain docs.
    assert!(docs.len() >= 6, "everything under the root is indexed: {docs:#?}");
}

#[test]
fn a_large_directory_indexes_markdown_without_copying_unrelated_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let corpus = dir.path().join("corpus");
    std::fs::create_dir(&corpus).unwrap();
    std::fs::write(corpus.join("notes.md"), "# Notes\n").unwrap();

    // A sparse file models a multi-GB video/archive without consuming that
    // much test disk. Adding the directory must never read or snapshot it.
    let asset = std::fs::File::create(corpus.join("archive.bin")).unwrap();
    asset.set_len(2 * 1024 * 1024 * 1024).unwrap();

    let folio = Folio::open(&dir.path().join("store")).unwrap();
    let result = folio
        .dispatch(
            &you(),
            "add_root",
            &json!({ "path": corpus.to_string_lossy() }),
        )
        .unwrap();

    assert_eq!(result["indexed"], 1);
    let docs = folio.dispatch(&you(), "list_docs", &json!({})).unwrap();
    assert_eq!(docs["docs"].as_array().unwrap().len(), 1);
    assert_eq!(docs["docs"][0]["relative"], "notes.md");
}

#[test]
fn a_path_escape_is_rejected() {
    let bed = Bed::new();
    for escape in [
        "../outside.md",
        "markdowns/../../outside.md",
        "C:/Windows/System32/drivers/etc/hosts",
        "/etc/passwd",
    ] {
        let err = bed
            .try_call(&agent(), "read_doc", json!({ "path": escape }))
            .expect_err(&format!("`{escape}` must not resolve"));
        assert!(
            matches!(err.code(), "outside_root" | "not_found" | "invalid"),
            "`{escape}` gave {}: {err}",
            err.code()
        );
    }
}

#[test]
fn search_finds_matches_with_line_numbers() {
    let bed = Bed::new();
    let result = bed.call(
        &agent(),
        "search_docs",
        json!({ "query": "threshold", "glob": "skills/**" }),
    );
    let matches = result["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1);
    assert!(matches[0]["text"].as_str().unwrap().contains("The threshold"));
    assert!(matches[0]["line"].as_u64().unwrap() > 1);
}

// ---------------------------------------------------------------------------
// Proposals
// ---------------------------------------------------------------------------

#[test]
fn an_agent_edit_arrives_as_a_reviewable_proposal_end_to_end() {
    let bed = Bed::new();
    let path = bed.path("markdowns/DESIGN.md");

    // The agent reads, then writes.
    let doc = bed.call(&agent(), "read_doc", json!({ "path": &path }));
    let original = doc["content"].as_str().unwrap().to_string();
    let proposed = original.replace(
        "The second paragraph, which will be rewritten.",
        "The second paragraph, rewritten by an agent.",
    );

    let outcome = bed.call(
        &agent(),
        "propose_edit",
        json!({ "path": &path, "content": proposed, "message": "Tighten the second paragraph" }),
    );
    assert_eq!(outcome["outcome"], "proposed");
    let proposal_id = outcome["proposal"]["id"].as_str().unwrap().to_string();

    // Nothing has touched the file.
    assert_eq!(bed.read("markdowns/DESIGN.md"), original);

    // The reviewer sees it, with a prose diff.
    let pending = bed.call(&you(), "list_proposals", json!({ "status": "pending" }));
    assert_eq!(pending["count"], 1);

    let review = bed.call(&you(), "proposal_diff", json!({ "id": &proposal_id }));
    let hunks = review["diff"]["hunks"].as_array().unwrap();
    assert_eq!(hunks.len(), 1, "one paragraph changed, one hunk");

    // Accepting applies it and records the agent as the author.
    let decision = bed.call(&you(), "accept_proposal", json!({ "id": &proposal_id }));
    assert_eq!(decision["proposal"]["status"], "accepted");
    assert_eq!(decision["snapshot"]["author"], "claude-sonnet-4.6");
    assert_eq!(decision["snapshot"]["source"], "proposal");
    assert!(bed.read("markdowns/DESIGN.md").contains("rewritten by an agent"));
}

#[test]
fn hunk_level_accept_applies_exactly_the_accepted_hunks() {
    let bed = Bed::new();
    let path = bed.path("markdowns/DESIGN.md");
    let original = bed.read("markdowns/DESIGN.md");
    let proposed = original
        .replace("The first paragraph", "The FIRST paragraph")
        .replace("The third paragraph", "The THIRD paragraph");

    let outcome = bed.call(
        &agent(),
        "propose_edit",
        json!({ "path": &path, "content": proposed, "message": "shout twice" }),
    );
    let id = outcome["proposal"]["id"].as_str().unwrap().to_string();

    let review = bed.call(&you(), "proposal_diff", json!({ "id": &id }));
    assert_eq!(review["diff"]["hunks"].as_array().unwrap().len(), 2);

    bed.call(&you(), "accept_proposal", json!({ "id": &id, "hunks": [1] }));
    let after = bed.read("markdowns/DESIGN.md");
    assert!(after.contains("The first paragraph"), "hunk 0 was rejected");
    assert!(after.contains("The THIRD paragraph"), "hunk 1 was accepted");
}

#[test]
fn a_rejection_note_is_the_feedback_loop() {
    let bed = Bed::new();
    let path = bed.path("markdowns/DESIGN.md");
    let proposed = bed.read("markdowns/DESIGN.md").replace("Design", "DESIGN");

    let outcome = bed.call(
        &agent(),
        "propose_edit",
        json!({ "path": &path, "content": proposed, "message": "shout the title" }),
    );
    let id = outcome["proposal"]["id"].as_str().unwrap().to_string();

    bed.call(
        &you(),
        "reject_proposal",
        json!({ "id": &id, "note": "Keep the title as it is; shouting is not tightening." }),
    );

    // The agent reads its own rejections back.
    let mine = bed.call(
        &agent(),
        "list_proposals",
        json!({ "status": "rejected", "mine_only": true }),
    );
    assert_eq!(mine["count"], 1);
    assert!(mine["proposals"][0]["decision_note"]
        .as_str()
        .unwrap()
        .contains("shouting is not tightening"));
}

#[test]
fn a_proposal_queued_headless_is_waiting_later() {
    let dir = tempfile::tempdir().unwrap();
    let corpus = dir.path().join("corpus");
    std::fs::create_dir_all(&corpus).unwrap();
    std::fs::write(corpus.join("NOTE.md"), "one\n\ntwo\n").unwrap();
    let store = dir.path().join("store");

    // Session one: no app, an agent proposes, the process ends.
    let proposal_id = {
        let folio = Folio::open(&store).unwrap();
        folio
            .dispatch(&you(), "add_root", &json!({ "path": corpus.to_string_lossy() }))
            .unwrap();
        let outcome = folio
            .dispatch(
                &agent(),
                "propose_edit",
                &json!({
                    "path": corpus.join("NOTE.md").to_string_lossy(),
                    "content": "one\n\nTWO\n",
                    "message": "overnight",
                }),
            )
            .unwrap();
        outcome["proposal"]["id"].as_str().unwrap().to_string()
    };

    // Session two: the app opens the next morning.
    let folio = Folio::open(&store).unwrap();
    let pending = folio
        .dispatch(&you(), "list_proposals", &json!({ "status": "pending" }))
        .unwrap();
    assert_eq!(pending["count"], 1);
    assert_eq!(pending["proposals"][0]["id"], proposal_id.as_str());
    assert_eq!(
        std::fs::read_to_string(corpus.join("NOTE.md")).unwrap(),
        "one\n\ntwo\n",
        "nothing was written while the app was closed"
    );
}

// ---------------------------------------------------------------------------
// Anchored comments
// ---------------------------------------------------------------------------

#[test]
fn a_comment_closes_the_loop_from_highlight_to_resolution() {
    let bed = Bed::new();
    let path = bed.path("skills/visual-explainer/SKILL.md");
    let content = bed.read("skills/visual-explainer/SKILL.md");

    let anchor = "The threshold: if the table has 4+ rows or 3+ columns, it belongs in the browser.";
    let start = content.find(anchor).unwrap();

    // You highlight and comment.
    let created = bed.call(
        &you(),
        "create_comment",
        json!({
            "path": &path,
            "selection_start": start,
            "selection_end": start + anchor.len(),
            "body": "4+ is too aggressive; two-column comparisons render fine inline. Make it 4 rows AND 4 columns.",
        }),
    );
    let comment_id = created["comment"]["id"].as_str().unwrap().to_string();
    assert_eq!(created["comment"]["status"], "open");

    // The agent finds it as a work item and replies.
    let inbox = bed.call(&agent(), "list_comments", json!({ "status": "open" }));
    assert_eq!(inbox["count"], 1);
    assert_eq!(inbox["comments"][0]["id"], comment_id.as_str());

    bed.call(
        &agent(),
        "reply_comment",
        json!({
            "comment_id": &comment_id,
            "body": "Tightened to 4 rows and 4 columns. A proposal addresses this.",
        }),
    );

    // The agent proposes an edit addressing the comment.
    let fixed = content.replace("4+ rows or 3+ columns", "4 rows and 4 columns");
    let outcome = bed.call(
        &agent(),
        "propose_edit",
        json!({
            "path": &path,
            "content": fixed,
            "message": "Tighten the proactive-table threshold",
            "addressing": &comment_id,
        }),
    );
    let proposal_id = outcome["proposal"]["id"].as_str().unwrap().to_string();

    // The proposal card carries the comment it answers.
    let review = bed.call(&you(), "proposal_diff", json!({ "id": &proposal_id }));
    assert_eq!(review["addresses"]["id"], comment_id.as_str());

    // Accepting closes the thread.
    let decision = bed.call(&you(), "accept_proposal", json!({ "id": &proposal_id }));
    assert_eq!(decision["resolved_comment"], comment_id.as_str());

    let resolved = bed.call(&you(), "get_comment", json!({ "comment_id": &comment_id }));
    assert_eq!(resolved["comment"]["status"], "resolved");
    assert_eq!(resolved["comment"]["resolved_by"], "you");
    assert_eq!(resolved["comment"]["addressed_by_proposal"], proposal_id.as_str());
    assert!(bed.read("skills/visual-explainer/SKILL.md").contains("4 rows and 4 columns"));
}

#[test]
fn rejecting_an_addressing_proposal_leaves_the_thread_open_with_the_note_in_it() {
    let bed = Bed::new();
    let path = bed.path("skills/visual-explainer/SKILL.md");
    let content = bed.read("skills/visual-explainer/SKILL.md");
    let anchor = "The threshold: if the table has 4+ rows or 3+ columns, it belongs in the browser.";
    let start = content.find(anchor).unwrap();

    let created = bed.call(
        &you(),
        "create_comment",
        json!({
            "path": &path,
            "selection_start": start,
            "selection_end": start + anchor.len(),
            "body": "Reconsider the threshold.",
        }),
    );
    let comment_id = created["comment"]["id"].as_str().unwrap().to_string();

    let outcome = bed.call(
        &agent(),
        "propose_edit",
        json!({
            "path": &path,
            "content": content.replace("4+ rows or 3+ columns", "2 rows or 2 columns"),
            "message": "Loosen it a lot",
            "addressing": &comment_id,
        }),
    );
    let proposal_id = outcome["proposal"]["id"].as_str().unwrap().to_string();

    bed.call(
        &you(),
        "reject_proposal",
        json!({ "id": &proposal_id, "note": "Too loose. Try 4 rows AND 4 columns." }),
    );

    let thread = bed.call(&you(), "get_comment", json!({ "comment_id": &comment_id }));
    assert_eq!(thread["comment"]["status"], "open", "rejection leaves it open");
    let replies = thread["comment"]["replies"].as_array().unwrap();
    assert!(
        replies.iter().any(|r| r["body"].as_str().unwrap().contains("Too loose")),
        "the rejection note is filed into the thread: {replies:#?}"
    );
}

#[test]
fn a_comment_survives_its_paragraph_moving_and_goes_outdated_when_edited() {
    let bed = Bed::new();
    let path = bed.path("markdowns/DESIGN.md");
    let content = bed.read("markdowns/DESIGN.md");
    let anchor = "The second paragraph, which will be rewritten.";
    let start = content.find(anchor).unwrap();

    let created = bed.call(
        &you(),
        "create_comment",
        json!({
            "path": &path,
            "selection_start": start,
            "selection_end": start + anchor.len(),
            "body": "This one needs work.",
        }),
    );
    let comment_id = created["comment"]["id"].as_str().unwrap().to_string();

    // The paragraph moves verbatim, with new prose above it.
    let moved = format!(
        "# Design\n\nA brand new opening that did not exist before.\n\n{anchor}\n\n\
         The first paragraph of the design document.\n\nThe third paragraph, left alone.\n"
    );
    bed.call(&you(), "save_doc", json!({ "path": &path, "content": moved }));

    let after_move = bed.call(&you(), "get_comment", json!({ "comment_id": &comment_id }));
    assert_eq!(after_move["comment"]["status"], "open", "a moved anchor still pins");
    assert!(after_move["comment"]["anchor"].is_object());

    // Now the anchored text itself changes.
    let edited = moved.replace(anchor, "The second paragraph, now entirely different.");
    bed.call(&you(), "save_doc", json!({ "path": &path, "content": edited }));

    let after_edit = bed.call(&you(), "get_comment", json!({ "comment_id": &comment_id }));
    assert_eq!(after_edit["comment"]["status"], "outdated", "never silently dropped");

    let outdated = bed.call(&you(), "list_comments", json!({ "status": "outdated" }));
    assert_eq!(outdated["count"], 1);
}

// ---------------------------------------------------------------------------
// Artifact intelligence
// ---------------------------------------------------------------------------

#[test]
fn a_real_skill_tree_validates_and_reports_reference_integrity() {
    let bed = Bed::new();
    let path = bed.path("skills/visual-explainer/SKILL.md");

    let clean = bed.call(&agent(), "validate_doc", json!({ "path": &path }));
    assert_eq!(clean["type"], "skill");
    assert_eq!(clean["errors"], 0, "the fixture skill is sound: {clean:#?}");

    // Break a reference and the finding is specific and located.
    let content = bed.read("skills/visual-explainer/SKILL.md")
        .replace("references/palette.md", "references/gone.md");
    bed.call(&you(), "save_doc", json!({ "path": &path, "content": content }));

    let broken = bed.call(&agent(), "validate_doc", json!({ "path": &path }));
    let findings = broken["findings"].as_array().unwrap();
    assert!(findings.iter().any(|f| f["rule"] == "skill.reference.missing"));
    assert!(
        findings.iter().any(|f| f["rule"] == "skill.reference.orphan"
            && f["file"] == "references/palette.md"),
        "the now-unreferenced file is reported: {findings:#?}"
    );
    assert!(broken["errors"].as_u64().unwrap() > 0);
}

#[test]
fn a_prompt_with_an_undeclared_slot_is_flagged_and_renders_once_fixed() {
    let bed = Bed::new();
    let path = bed.path("prompts/brief.md");

    let report = bed.call(&agent(), "validate_doc", json!({ "path": &path }));
    assert_eq!(report["type"], "prompt");
    let findings = report["findings"].as_array().unwrap();
    assert!(
        findings.iter().any(|f| f["rule"] == "prompt.slot.undeclared"
            && f["message"].as_str().unwrap().contains("tone")),
        "{findings:#?}"
    );

    // Rendering with a hole is refused.
    let err = bed
        .try_call(&agent(), "render_prompt", json!({ "path": &path, "variables": { "topic": "otters" } }))
        .unwrap_err();
    assert_eq!(err.code(), "invalid");
    assert!(err.to_string().contains("tone"));

    let rendered = bed.call(
        &agent(),
        "render_prompt",
        json!({ "path": &path, "variables": { "topic": "otters", "tone": "dry" } }),
    );
    assert_eq!(rendered["rendered"], "Write about otters in a dry voice.\n");
}

// ---------------------------------------------------------------------------
// Task lists and Today
// ---------------------------------------------------------------------------

#[test]
fn an_agent_maintains_the_task_list_directly_and_it_shows_up_in_today() {
    let bed = Bed::new();
    let todo = bed.path("TODO.md");

    // The default policy for a task list is `direct`: an agent maintaining
    // TODO.md overnight must not block on review.
    let added = bed.call(
        &agent(),
        "task_add",
        json!({ "doc": &todo, "text": "Write the release notes", "owner": "carlos", "tag": "release" }),
    );
    assert_eq!(added["outcome"], "applied");
    assert!(bed.read("TODO.md").contains("- [ ] Write the release notes @carlos #release"));

    // Read, then write.
    let listed = bed.call(&agent(), "task_list", json!({ "doc": &todo, "open_only": true }));
    let tasks = listed["tasks"].as_array().unwrap();
    let ship = tasks
        .iter()
        .find(|t| t["text"] == "Ship Folio 1.0")
        .expect("the open task is listed");
    let version = ship["version"].as_str().unwrap().to_string();

    bed.call(
        &agent(),
        "task_set_status",
        json!({
            "doc": &todo,
            "task_id": ship["id"],
            "status": "done",
            "version": version,
            "text": "Ship Folio 1.0",
        }),
    );
    assert!(bed.read("TODO.md").contains("- [x] Ship Folio 1.0 @carlos #release"));

    // Today aggregates what is left, grouped by owner and tag.
    let today = bed.call(&you(), "today", json!({}));
    assert_eq!(today["tasks"]["open"], 2);
    let by_owner = today["tasks"]["by_owner"].as_object().unwrap();
    assert_eq!(by_owner["carlos"].as_array().unwrap().len(), 2);
    let by_tag = today["tasks"]["by_tag"].as_object().unwrap();
    assert_eq!(by_tag["release"].as_array().unwrap().len(), 2);

    // Overnight changes are grouped by author.
    let by_author = today["changes"]["by_author"].as_object().unwrap();
    assert!(by_author.contains_key("claude-sonnet-4.6"), "{by_author:#?}");
}

#[test]
fn a_stale_task_write_fails_with_the_current_list_attached() {
    let bed = Bed::new();
    let todo = bed.path("TODO.md");

    let listed = bed.call(&agent(), "task_list", json!({ "doc": &todo }));
    let stale_version = listed["tasks"][0]["version"].as_str().unwrap().to_string();
    let task_id = listed["tasks"][0]["id"].as_str().unwrap().to_string();

    // Somebody else rewrites the list underneath.
    bed.call(
        &you(),
        "save_doc",
        json!({
            "path": &todo,
            "content": "# TODO\n\n- [ ] Something else entirely @carlos\n- [ ] Ship Folio 1.0 @carlos #release\n",
        }),
    );

    let err = bed
        .try_call(
            &agent(),
            "task_set_status",
            json!({ "doc": &todo, "task_id": &task_id, "status": "done", "version": &stale_version }),
        )
        .unwrap_err();

    assert_eq!(err.code(), "stale");
    let data = err.data().expect("the current list must be attached");
    let tasks = data["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 2, "the retry needs no second read: {data:#?}");
    assert_ne!(data["version"].as_str().unwrap(), stale_version);
}

// ---------------------------------------------------------------------------
// Versioning
// ---------------------------------------------------------------------------

#[test]
fn versioning_is_ambient_and_restores_are_undoable() {
    let bed = Bed::new();
    let path = bed.path("markdowns/DESIGN.md");

    let first = bed.call(&you(), "list_versions", json!({ "path": &path }));
    let baseline = first["versions"].as_array().unwrap().len();
    assert_eq!(baseline, 1, "indexing a root records one version per file");

    // Re-saving unchanged content creates nothing.
    let unchanged = bed.call(
        &you(),
        "save_doc",
        json!({ "path": &path, "content": bed.read("markdowns/DESIGN.md") }),
    );
    assert_eq!(unchanged["created"], false);

    bed.call(&you(), "save_doc", json!({ "path": &path, "content": "# Design\n\nRewritten.\n" }));
    bed.call(&you(), "checkpoint", json!({ "path": &path, "message": "before restructuring" }));

    let timeline = bed.call(&you(), "list_versions", json!({ "path": &path }));
    let versions = timeline["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 3);
    assert_eq!(versions[0]["source"], "checkpoint");
    assert_eq!(versions[0]["message"], "before restructuring");

    // Restore the original, and the restore is itself a version.
    let oldest = versions.last().unwrap()["id"].as_str().unwrap().to_string();
    bed.call(&you(), "restore_version", json!({ "path": &path, "version_id": oldest }));
    assert_eq!(bed.read("markdowns/DESIGN.md"), DESIGN_MD);

    let after = bed.call(&you(), "list_versions", json!({ "path": &path }));
    assert_eq!(after["versions"].as_array().unwrap().len(), 4);
    assert_eq!(after["versions"][0]["source"], "restore");
}

#[test]
fn history_exports_as_a_patch_series_without_touching_git() {
    let bed = Bed::new();
    let path = bed.path("markdowns/DESIGN.md");
    bed.call(&you(), "save_doc", json!({ "path": &path, "content": "# Design\n\nSecond version.\n" }));

    let export = bed.call(&you(), "export_history", json!({ "path": &path }));
    let series = export["patch_series"].as_str().unwrap();
    assert!(series.contains("Subject: [PATCH 1/2]"));
    assert!(series.contains("diff --git"));
    assert!(!bed.dir.path().join("corpus").join(".git").exists());
}

// ---------------------------------------------------------------------------
// Policy
// ---------------------------------------------------------------------------

#[test]
fn write_policy_is_enforced_in_the_core_and_overridable_per_path() {
    let bed = Bed::new();
    let roots = bed.call(&you(), "list_roots", json!({}));
    let root_id = roots["roots"][0]["id"].as_str().unwrap().to_string();

    // A doc proposes by default.
    let doc_path = bed.path("markdowns/DESIGN.md");
    let outcome = bed.call(
        &agent(),
        "propose_edit",
        json!({ "path": &doc_path, "content": "# Design\n\nChanged.\n" }),
    );
    assert_eq!(outcome["outcome"], "proposed");

    // Pin that one path to `direct` and the same call now writes through.
    bed.call(
        &you(),
        "set_path_policy",
        json!({ "root_id": &root_id, "pattern": "markdowns/**", "policy": "direct" }),
    );
    let outcome = bed.call(
        &agent(),
        "propose_edit",
        json!({ "path": &doc_path, "content": "# Design\n\nChanged directly.\n" }),
    );
    assert_eq!(outcome["outcome"], "applied");
    assert!(bed.read("markdowns/DESIGN.md").contains("Changed directly"));

    // And the reverse: pin the task list to `propose` and it stops applying.
    bed.call(
        &you(),
        "set_path_policy",
        json!({ "root_id": &root_id, "pattern": "TODO.md", "policy": "propose" }),
    );
    let outcome = bed.call(
        &agent(),
        "task_add",
        json!({ "doc": bed.path("TODO.md"), "text": "Gated now" }),
    );
    assert_eq!(outcome["outcome"], "proposed");
    assert!(!bed.read("TODO.md").contains("Gated now"));
}

#[test]
fn agents_cannot_decide_proposals_or_resolve_threads() {
    let bed = Bed::new();
    let path = bed.path("markdowns/DESIGN.md");
    let outcome = bed.call(
        &agent(),
        "propose_edit",
        json!({ "path": &path, "content": "# Design\n\nChanged.\n" }),
    );
    let id = outcome["proposal"]["id"].as_str().unwrap().to_string();

    for (op, params) in [
        ("accept_proposal", json!({ "id": &id })),
        ("reject_proposal", json!({ "id": &id, "note": "no" })),
        ("save_doc", json!({ "path": &path, "content": "sneaky" })),
        ("restore_version", json!({ "path": &path, "version_id": "snap_x" })),
    ] {
        let err = bed.try_call(&agent(), op, params).expect_err(&format!("{op} must be human-only"));
        assert_eq!(err.code(), "policy_denied", "{op} gave {err}");
    }

    // And the file is untouched by any of it.
    assert_eq!(bed.read("markdowns/DESIGN.md"), DESIGN_MD);
}

#[test]
fn usage_instrumentation_is_opt_in_context_free_and_human_controlled() {
    let bed = Bed::new();

    let initial = bed.call(&you(), "instrumentation_status", json!({}));
    assert_eq!(initial, json!({ "enabled": false, "events": 0 }));
    assert_eq!(
        bed.call(
            &you(),
            "record_usage_event",
            json!({ "session": "launch-1", "event": "view.today" }),
        )["recorded"],
        false
    );

    let denied = bed
        .try_call(
            &agent(),
            "set_instrumentation",
            json!({ "enabled": true }),
        )
        .unwrap_err();
    assert_eq!(denied.code(), "policy_denied");

    bed.call(
        &you(),
        "set_instrumentation",
        json!({ "enabled": true }),
    );
    bed.call(
        &you(),
        "record_usage_event",
        json!({ "session": "launch-1", "event": "view.today" }),
    );
    let export = bed.call(&you(), "export_usage_events", json!({}));
    assert_eq!(export["schema"], 1);
    assert_eq!(export["events"].as_array().unwrap().len(), 1);
    assert_eq!(export["events"][0]["event"], "view.today");
    assert_eq!(export["events"][0]["session"], "launch-1");
    assert_eq!(export["events"][0]["app_version"], folio_core::VERSION);
    assert!(export["events"][0].get("properties").is_none());

    assert_eq!(
        bed.call(&you(), "clear_usage_events", json!({}))["cleared"],
        1
    );
    assert_eq!(
        bed.call(&you(), "instrumentation_status", json!({}))["events"],
        0
    );
}


#[test]
fn emptying_a_document_is_a_legitimate_edit() {
    let bed = Bed::new();
    let path = bed.path("markdowns/DESIGN.md");

    // An empty string is a value, not an absence: the editor must be able to
    // clear a file, and an agent must be able to propose clearing one.
    let outcome = bed.call(
        &agent(),
        "propose_edit",
        json!({ "path": &path, "content": "", "message": "empty it" }),
    );
    let id = outcome["proposal"]["id"].as_str().unwrap().to_string();
    bed.call(&you(), "accept_proposal", json!({ "id": id }));
    assert_eq!(bed.read("markdowns/DESIGN.md"), "");

    bed.call(&you(), "save_doc", json!({ "path": &path, "content": "# Back\n" }));
    assert_eq!(bed.read("markdowns/DESIGN.md"), "# Back\n");
}
