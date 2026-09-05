//! Dev-only: put a couple of anchored comment threads into a store.
//!
//! Comments are created by the human from the UI, never over MCP, so the demo
//! seeder cannot make them through the agent path. This walks the same
//! `dispatch` table the UI does, as the human caller.
//!
//!     cargo run -p folio-core --example seed_comments -- <store-dir> <corpus-dir>

use folio_core::api::{Caller, Folio};
use serde_json::json;

fn main() {
    let mut args = std::env::args().skip(1);
    let store = args.next().expect("usage: seed_comments <store-dir> <corpus-dir>");
    let corpus = args.next().expect("usage: seed_comments <store-dir> <corpus-dir>");

    let folio = Folio::open(std::path::Path::new(&store)).expect("open store");
    let you = Caller::human();

    let skill = format!("{corpus}/skills/visual-explainer/SKILL.md");
    let spec = format!("{corpus}/markdowns/FOLIO_SPEC.md");

    // A thread an agent has already replied to and addressed with a proposal.
    let anchor = "The threshold: if the table has 4+ rows or 3+ columns, it belongs in the browser.";
    let comment = comment_on(
        &folio,
        &you,
        &skill,
        anchor,
        "4+ is too aggressive; two-column comparisons render fine inline. Make it 4 rows AND 4 columns.",
    );

    if let Some(id) = &comment {
        folio
            .dispatch(
                &Caller::agent("claude-sonnet-4.6", "claude-code"),
                "reply_comment",
                &json!({
                    "comment_id": id,
                    "body": "Tightened to 4 rows and 4 columns. The proposal on this file addresses it.",
                }),
            )
            .expect("reply");

        // Link the pending proposal for this file to the thread, the way an
        // agent would by passing `addressing`.
        let pending = folio
            .dispatch(&you, "list_proposals", &json!({ "status": "pending" }))
            .expect("list proposals");
        let target = pending["proposals"]
            .as_array()
            .and_then(|list| {
                list.iter()
                    .find(|p| p["path"].as_str().is_some_and(|path| path.ends_with("SKILL.md")))
                    .cloned()
            });
        if let Some(proposal) = target {
            let path = proposal["path"].as_str().unwrap_or_default().to_string();
            let content = folio
                .dispatch(&you, "proposal_diff", &json!({ "id": proposal["id"] }))
                .ok()
                .and_then(|d| d["proposed"].as_str().map(str::to_string));
            if let Some(content) = content {
                // Re-propose with the link, superseding the unlinked one.
                let _ = folio.dispatch(
                    &Caller::agent("claude-sonnet-4.6", "claude-code"),
                    "propose_edit",
                    &json!({
                        "path": path,
                        "content": content,
                        "message": proposal["message"],
                        "addressing": id,
                    }),
                );
            }
        }
    }

    // A second, plain open thread, and one that will read as outdated.
    comment_on(
        &folio,
        &you,
        &spec,
        "Direct writes are an opt-in policy, not the default.",
        "Say why: the asymmetry is the retention hook, and it deserves a sentence here.",
    );

    println!("seeded comment threads");
}

fn comment_on(
    folio: &Folio,
    you: &Caller,
    path: &str,
    anchor: &str,
    body: &str,
) -> Option<String> {
    let doc = folio.dispatch(you, "read_doc", &json!({ "path": path })).ok()?;
    let content = doc["content"].as_str()?;
    let start = content.find(anchor)?;
    let created = folio
        .dispatch(
            you,
            "create_comment",
            &json!({
                "path": path,
                "selection_start": start,
                "selection_end": start + anchor.len(),
                "body": body,
            }),
        )
        .ok()?;
    created["comment"]["id"].as_str().map(str::to_string)
}
