/* The review view: the proposal inbox, full screen.
 *
 * Proposals on the left grouped by author, a prose diff on the right with
 * per-hunk accept/reject. A proposal that addresses a comment shows that
 * comment inline, and accepting it resolves the thread.
 */
(function (global) {
    'use strict';

    const { $, el, escapeHtml } = global.UI;

    let app = null;
    let proposals = [];
    let selectedId = null;
    let currentDiff = null;
    let statusFilter = 'open';

    function isOpen(p) {
        return p.status === 'pending' || p.status === 'conflict';
    }

    async function load(selectId) {
        const result = await global.Folio.tryCall('list_proposals', {}, { proposals: [] });
        proposals = result.proposals || [];
        app.setProposals(proposals);

        const visible = visibleProposals();
        if (selectId && proposals.some((p) => p.id === selectId)) selectedId = selectId;
        if (!selectedId || !visible.some((p) => p.id === selectedId)) {
            selectedId = visible.length ? visible[0].id : null;
        }

        renderList();
        await renderDetail();
        renderFooter();
    }

    function visibleProposals() {
        if (statusFilter === 'open') return proposals.filter(isOpen);
        if (statusFilter === 'all') return proposals;
        return proposals.filter((p) => p.status === statusFilter);
    }

    function renderList() {
        const host = $('review-files');
        host.innerHTML = '';

        const filters = el('div', 'review-actions');
        filters.style.padding = '2px 4px 8px';
        for (const [value, label] of [['open', 'Open'], ['rejected', 'Rejected'], ['accepted', 'Accepted'], ['all', 'All']]) {
            const button = el('button', 'hunk-btn' + (statusFilter === value ? ' chosen-accept' : ''), label);
            button.type = 'button';
            button.addEventListener('click', async () => {
                statusFilter = value;
                selectedId = null;
                await load();
            });
            filters.appendChild(button);
        }
        host.appendChild(filters);

        const visible = visibleProposals();
        if (!visible.length) {
            const empty = el('div', 'drawer-empty');
            empty.innerHTML = statusFilter === 'open'
                ? 'Nothing waiting for review.<br>Agent edits arrive here as proposals.'
                : 'No proposals with that status.';
            host.appendChild(empty);
            return;
        }

        // Grouped by author: the morning review reads "what did each agent do".
        const groups = new Map();
        for (const proposal of visible) {
            if (!groups.has(proposal.author)) groups.set(proposal.author, []);
            groups.get(proposal.author).push(proposal);
        }

        for (const [author, items] of groups) {
            host.appendChild(el('div', 'review-group-label', author + ' · ' + items.length));
            for (const proposal of items) {
                const row = el('button', 'review-row');
                row.type = 'button';
                if (proposal.id === selectedId) row.classList.add('active');

                const badgeChar = proposal.status === 'conflict' ? '!' :
                    proposal.status === 'accepted' ? '✓' :
                    proposal.status === 'rejected' ? '✕' :
                    proposal.status === 'superseded' ? '~' : '●';
                row.appendChild(el('span', 'review-row-badge review-row-badge--' + proposal.status, badgeChar));

                const main = el('div', 'review-row-main');
                main.appendChild(el('div', 'review-row-path', shortPath(proposal.display)));
                main.appendChild(el('div', 'review-row-meta',
                    (proposal.message || 'no message') + ' · ' + global.UI.relativeTime(proposal.created_at_iso)));
                row.appendChild(main);

                const counts = el('span', 'review-row-counts');
                if (proposal.stats) {
                    counts.innerHTML = '<span style="color:var(--success-color)">+' + proposal.stats.added +
                        '</span> <span style="color:var(--error-color)">−' + proposal.stats.removed + '</span>';
                }
                row.appendChild(counts);

                row.addEventListener('click', async () => {
                    selectedId = proposal.id;
                    renderList();
                    await renderDetail();
                    renderFooter();
                });
                host.appendChild(row);
            }
        }
    }

    function shortPath(display) {
        const parts = display.split('/');
        return parts.length <= 3 ? display : '…/' + parts.slice(-3).join('/');
    }

    async function renderDetail() {
        const host = $('review-detail');
        host.innerHTML = '';
        currentDiff = null;

        if (!selectedId) {
            const empty = el('div', 'empty-state');
            empty.innerHTML =
                '<h2>Nothing to review</h2>' +
                '<p>When an agent edits through MCP, its change lands here as a proposal ' +
                'instead of overwriting the file. Review it hunk by hunk, then accept or reject.</p>';
            host.appendChild(empty);
            return;
        }

        let payload;
        try {
            payload = await global.Folio.call('proposal_diff', { id: selectedId });
        } catch (e) {
            host.appendChild(el('div', 'diff-empty', e.message));
            return;
        }

        const proposal = payload.proposal;

        const head = el('div', 'proposal-head');
        const title = el('div', 'proposal-head-title');
        title.appendChild(el('span', 'proposal-path', proposal.display));
        title.appendChild(el('span', 'status-chip status-chip--' + proposal.status, proposal.status));
        title.appendChild(el('span', 'proposal-author', proposal.author + ' via ' + proposal.client));
        head.appendChild(title);

        if (proposal.message) head.appendChild(el('div', 'proposal-message', proposal.message));

        const meta = el('div', 'proposal-meta');
        meta.innerHTML =
            escapeHtml(proposal.id) + ' · proposed ' + escapeHtml(global.UI.absoluteTime(proposal.created_at_iso)) +
            (proposal.base_snapshot_id ? ' · base ' + escapeHtml(proposal.base_snapshot_id) : ' · new file') +
            (proposal.decided_at_iso ? ' · decided ' + escapeHtml(global.UI.absoluteTime(proposal.decided_at_iso)) : '');
        head.appendChild(meta);

        if (proposal.status === 'conflict') {
            const banner = el('div', 'proposal-conflict');
            banner.appendChild(el('span', null,
                'The file changed on disk after this proposal was made. Rebase it onto the current content, or reject it.'));
            const rebase = el('button', 'hunk-btn', 'Rebase');
            rebase.type = 'button';
            rebase.addEventListener('click', () => rebaseProposal(proposal.id));
            banner.appendChild(rebase);
            head.appendChild(banner);
        }

        if (proposal.target_missing) {
            const banner = el('div', 'proposal-conflict');
            banner.textContent = 'The target document no longer exists on disk. Accepting will recreate it.';
            head.appendChild(banner);
        }

        if (proposal.decision_note) {
            const note = el('div', 'proposal-note');
            note.innerHTML = '<strong>Your note to the agent:</strong> ' + escapeHtml(proposal.decision_note);
            head.appendChild(note);
        }

        // The comment this proposal answers, shown inline. This is the loop
        // closing: highlight, comment, agent proposes, you accept.
        if (payload.addresses) {
            const comment = payload.addresses;
            const card = el('div', 'addressed-comment');
            card.appendChild(el('span', 'addressed-comment-label', 'Addresses ' + comment.id));
            card.appendChild(el('div', 'comment-anchor', comment.excerpt));
            card.appendChild(el('div', 'comment-body', comment.author + ': ' + comment.body));
            if (comment.replies.length) {
                const last = comment.replies[comment.replies.length - 1];
                const reply = el('div', 'comment-reply');
                reply.appendChild(el('div', 'comment-reply-head', last.author));
                reply.appendChild(el('div', 'comment-reply-body', last.body));
                card.appendChild(reply);
            }
            head.appendChild(card);
        }

        host.appendChild(head);

        const diffHost = el('div', 'diff');
        host.appendChild(diffHost);
        const reviewable = isOpen(proposal);
        currentDiff = global.DiffView.render(diffHost, payload.diff, {
            review: reviewable,
            onSelectionChange: () => renderFooter(),
        });
        currentDiff.proposal = proposal;
        currentDiff.hunkCount = payload.diff && payload.diff.hunks ? payload.diff.hunks.length : 0;
    }

    function renderFooter() {
        const summary = $('review-summary');
        const acceptBtn = $('review-accept-all');
        const rejectBtn = $('review-reject-all');

        const openCount = proposals.filter(isOpen).length;
        const badge = $('review-badge');
        badge.textContent = String(openCount);
        badge.hidden = openCount === 0;

        if (!currentDiff || !currentDiff.proposal || !isOpen(currentDiff.proposal)) {
            summary.textContent = openCount
                ? openCount + ' proposal' + (openCount === 1 ? '' : 's') + ' waiting'
                : 'Inbox clear';
            acceptBtn.disabled = true;
            rejectBtn.disabled = !currentDiff || !currentDiff.proposal || !isOpen(currentDiff.proposal);
            acceptBtn.textContent = 'Accept';
            return;
        }

        const accepted = currentDiff.accepted();
        const total = currentDiff.hunkCount;
        acceptBtn.disabled = accepted.length === 0;
        rejectBtn.disabled = false;
        acceptBtn.textContent = accepted.length === total ? 'Accept All' : 'Accept ' + accepted.length + ' of ' + total;
        summary.textContent = openCount + ' waiting · ' +
            (accepted.length === total
                ? 'accepting every hunk'
                : accepted.length + ' of ' + total + ' hunks selected');
    }

    async function acceptSelected() {
        if (!currentDiff || !currentDiff.proposal) return;
        const accepted = currentDiff.accepted();
        const partial = accepted.length !== currentDiff.hunkCount;
        try {
            const decision = await global.Folio.call('accept_proposal', {
                id: currentDiff.proposal.id,
                hunks: partial ? accepted : null,
            });
            const resolved = decision.resolved_comment
                ? ' Comment ' + decision.resolved_comment + ' resolved.'
                : '';
            global.UI.toast(
                (partial ? 'Applied ' + accepted.length + ' hunk(s).' : 'Proposal accepted.') + resolved,
                { type: 'success' }
            );
            selectedId = null;
            await load();
            app.refreshDocs();
            if (global.DocView.path() === decision.proposal.path) await global.DocView.reloadIfClean();
        } catch (e) {
            global.UI.error(e, 'Could not accept the proposal');
        }
    }

    async function rejectSelected() {
        if (!currentDiff || !currentDiff.proposal) return;
        const note = await global.UI.note(
            'Why is this being rejected? The note goes back to the agent — it is the whole feedback loop.',
            {
                title: 'Reject proposal',
                placeholder: 'Keep 4+; 3-row tables are fine inline. The KaTeX note is good, resubmit just that.',
                okLabel: 'Reject',
                danger: true,
            }
        );
        if (note === null) return;
        try {
            await global.Folio.call('reject_proposal', { id: currentDiff.proposal.id, note });
            global.UI.toast('Rejected. The agent will read your note on its next list_proposals.', { type: 'info' });
            selectedId = null;
            await load();
            app.refreshDocs();
        } catch (e) {
            global.UI.error(e, 'Could not reject the proposal');
        }
    }

    async function rebaseProposal(id) {
        try {
            await global.Folio.call('rebase_proposal', { id });
            global.UI.toast('Rebased onto the current content.', { type: 'success' });
            await load(id);
        } catch (e) {
            global.UI.error(e, 'Could not rebase');
        }
    }

    async function acceptEveryPending() {
        const open = proposals.filter(isOpen);
        if (!open.length) return;
        const go = await global.UI.confirm(
            'Accept all ' + open.length + ' waiting proposal(s) in full?\n\n' +
            'Each is applied to disk and recorded as a version authored by the agent that proposed it.',
            { title: 'Accept all', okLabel: 'Accept all' }
        );
        if (!go) return;

        global.UI.loading(true, 'Accepting proposals…');
        let applied = 0;
        const failures = [];
        for (const proposal of open) {
            try {
                await global.Folio.call('accept_proposal', { id: proposal.id });
                applied += 1;
            } catch (e) {
                failures.push(proposal.display + ': ' + e.message);
            }
        }
        global.UI.loading(false);
        await load();
        app.refreshDocs();
        await global.DocView.reloadIfClean();

        if (failures.length) {
            global.UI.alert(
                'Applied ' + applied + '. These could not be applied:\n\n' + failures.join('\n'),
                'Accept all'
            );
        } else {
            global.UI.toast('Applied ' + applied + ' proposal(s).', { type: 'success' });
        }
    }

    async function rejectEveryPending() {
        const open = proposals.filter(isOpen);
        if (!open.length) return;
        const note = await global.UI.note(
            'Reject all ' + open.length + ' waiting proposal(s). The note is filed against every one of them.',
            { title: 'Reject all', placeholder: 'Not now — I am restructuring these files myself.', okLabel: 'Reject all', danger: true }
        );
        if (note === null) return;

        global.UI.loading(true, 'Rejecting proposals…');
        for (const proposal of open) {
            await global.Folio.tryCall('reject_proposal', { id: proposal.id, note }, null);
        }
        global.UI.loading(false);
        await load();
        app.refreshDocs();
        global.UI.toast('Rejected ' + open.length + ' proposal(s).', { type: 'info' });
    }

    const Review = {
        init(application) {
            app = application;
            $('review-accept-all').addEventListener('click', acceptSelected);
            $('review-reject-all').addEventListener('click', rejectSelected);
        },
        load,
        acceptEveryPending,
        rejectEveryPending,
        select(id) {
            statusFilter = 'all';
            return load(id);
        },
        proposals: () => proposals,
    };

    global.Review = Review;
})(window);
