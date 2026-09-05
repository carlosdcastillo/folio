/* Today: the morning view, and the daily-driver test.
 *
 * Open tasks across the whole corpus grouped by @owner and #tag, open comment
 * threads with their latest replies, and every change since the last review
 * session grouped by author.
 */
(function (global) {
    'use strict';

    const { $, el, escapeHtml } = global.UI;

    let app = null;
    let data = null;

    async function load() {
        data = await global.Folio.tryCall('today', {}, null);
        render();
    }

    function render() {
        const host = $('today-body');
        host.innerHTML = '';

        if (!data) {
            host.appendChild(el('div', 'today-empty', 'Could not load the Today view.'));
            return;
        }

        const head = el('div', 'today-head');
        head.appendChild(el('h1', null, greeting()));
        head.appendChild(el('span', 'today-since',
            'since your last review, ' + global.UI.relativeTime(data.since_iso)));

        const actions = el('div', 'today-actions');
        const reviewed = el('button', 'toolbar-btn', 'Mark reviewed');
        reviewed.title = 'Reset the "overnight changes" window to now';
        reviewed.addEventListener('click', async () => {
            await global.Folio.tryCall('mark_reviewed', {}, null);
            await load();
            global.UI.toast('Marked reviewed. "Overnight" now starts from this moment.', { type: 'success' });
        });
        actions.appendChild(reviewed);
        head.appendChild(actions);
        host.appendChild(head);

        // -- metrics ---------------------------------------------------------
        const metrics = el('div', 'today-metrics');
        metrics.appendChild(metric(data.proposals.pending, 'proposals waiting',
            data.proposals.pending ? 'attention' : 'good'));
        metrics.appendChild(metric(data.tasks.open, 'open tasks'));
        metrics.appendChild(metric(data.comments.open, 'open comments',
            data.comments.open ? 'attention' : 'good'));
        metrics.appendChild(metric(data.changes.total, 'changes overnight'));
        host.appendChild(metrics);

        // -- proposals -------------------------------------------------------
        if (data.proposals.pending) {
            const section = section2('Waiting for review', data.proposals.pending);
            for (const proposal of data.proposals.all) {
                const row = el('button', 'change-row');
                row.type = 'button';
                const who = el('span', 'source-badge source-badge--proposal author-badge', proposal.author);
                who.title = proposal.author + ' via ' + proposal.client;
                row.appendChild(who);
                const main = el('div', 'change-path');
                main.appendChild(el('div', null, proposal.display));
                main.appendChild(el('div', 'change-message', proposal.message || 'no message'));
                row.appendChild(main);
                row.appendChild(el('span', 'timeline-delta timeline-delta--up',
                    proposal.stats ? '+' + proposal.stats.added + ' −' + proposal.stats.removed : ''));
                row.appendChild(el('span', 'timeline-time', global.UI.relativeTime(proposal.created_at_iso)));
                row.addEventListener('click', () => app.openReview(proposal.id));
                section.appendChild(row);
            }
            host.appendChild(section);
        }

        // -- tasks -----------------------------------------------------------
        const tasks = section2('Tasks', data.tasks.open);
        if (!data.tasks.open) {
            tasks.appendChild(el('div', 'today-empty',
                'No open tasks. Register a task list — a file that is mostly GFM checkboxes — and it will appear here.'));
        } else {
            const owners = Object.keys(data.tasks.by_owner).sort(unassignedLast);
            for (const owner of owners) {
                const group = el('div', 'today-group');
                group.appendChild(el('div', 'today-group-label',
                    owner === 'unassigned' ? 'unassigned' : '@' + owner));
                for (const task of data.tasks.by_owner[owner]) group.appendChild(taskRow(task));
                tasks.appendChild(group);
            }

            const tagNames = Object.keys(data.tasks.by_tag).filter((t) => data.tasks.by_tag[t].length);
            if (tagNames.length) {
                const byTag = el('div', 'today-group');
                byTag.appendChild(el('div', 'today-group-label', 'by tag'));
                const chips = el('div', 'review-actions');
                for (const tag of tagNames.sort()) {
                    const chip = el('button', 'hunk-btn', '#' + tag + ' · ' + data.tasks.by_tag[tag].length);
                    chip.type = 'button';
                    chip.addEventListener('click', () => {
                        const first = data.tasks.by_tag[tag][0];
                        if (first && first.doc) app.openDoc(first.doc);
                    });
                    chips.appendChild(chip);
                }
                byTag.appendChild(chips);
                tasks.appendChild(byTag);
            }
        }
        host.appendChild(tasks);

        // -- comments --------------------------------------------------------
        const totalThreads = data.comments.open + data.comments.outdated;
        const comments = section2('Comments', totalThreads);
        if (!totalThreads) {
            comments.appendChild(el('div', 'today-empty',
                'No open threads. Select a passage in any document and leave a note; ' +
                'connected agents read open comments as work items.'));
        } else {
            for (const thread of data.comments.outdated_threads.concat(data.comments.threads)) {
                comments.appendChild(threadCard(thread));
            }
        }
        host.appendChild(comments);

        // -- overnight changes ------------------------------------------------
        const changes = section2('Changed since your last review', data.changes.total);
        if (!data.changes.total) {
            changes.appendChild(el('div', 'today-empty', 'Nothing has changed.'));
        } else {
            const authors = Object.keys(data.changes.by_author).sort();
            for (const author of authors) {
                const group = el('div', 'today-group');
                group.appendChild(el('div', 'today-group-label',
                    author + ' · ' + data.changes.by_author[author].length));
                for (const snap of data.changes.by_author[author]) {
                    const row = el('button', 'change-row');
                    row.type = 'button';
                    row.appendChild(el('span', 'source-badge source-badge--' + snap.source, snap.source));
                    const main = el('div', 'change-path');
                    main.appendChild(el('div', null, snap.display));
                    if (snap.message) main.appendChild(el('div', 'change-message', snap.message));
                    row.appendChild(main);
                    row.appendChild(el('span', 'timeline-delta ' +
                        (snap.size_delta > 0 ? 'timeline-delta--up' : 'timeline-delta--down'),
                        global.UI.delta(snap.size_delta)));
                    row.appendChild(el('span', 'timeline-time', global.UI.relativeTime(snap.created_at_iso)));
                    row.addEventListener('click', () => app.openDoc(snap.path, { drawer: 'timeline' }));
                    group.appendChild(row);
                }
                changes.appendChild(group);
            }
        }
        host.appendChild(changes);
    }

    function unassignedLast(a, b) {
        if (a === 'unassigned') return 1;
        if (b === 'unassigned') return -1;
        return a.localeCompare(b);
    }

    function greeting() {
        const hour = new Date().getHours();
        if (hour < 5) return 'Tonight';
        if (hour < 12) return 'This morning';
        if (hour < 18) return 'Today';
        return 'This evening';
    }

    function metric(value, label, tone) {
        const node = el('div', 'metric' + (tone ? ' metric--' + tone : ''));
        node.appendChild(el('div', 'metric-value', String(value)));
        node.appendChild(el('div', 'metric-label', label));
        return node;
    }

    function section2(title, count) {
        const node = el('section', 'today-section');
        const heading = el('h2', null, title);
        heading.appendChild(el('span', 'count', String(count)));
        node.appendChild(heading);
        return node;
    }

    function taskRow(task) {
        const row = el('div', 'task-row' + (task.done ? ' done' : ''));

        const box = document.createElement('input');
        box.type = 'checkbox';
        box.checked = !!task.done;
        box.addEventListener('change', async () => {
            box.disabled = true;
            try {
                await global.Folio.call('task_set_status', {
                    doc: task.doc,
                    task_id: task.id,
                    status: box.checked ? 'done' : 'open',
                    version: task.version,
                    text: task.text,
                });
                await load();
                app.refreshDocs();
            } catch (e) {
                box.checked = !box.checked;
                // A stale write returns the current list attached; the honest
                // response is to reload rather than to retry blindly.
                if (e.code === 'stale') {
                    global.UI.toast('That list changed underneath. Reloaded.', { type: 'warning' });
                    await load();
                } else {
                    global.UI.error(e, 'Could not update the task');
                }
            } finally {
                box.disabled = false;
            }
        });
        row.appendChild(box);

        const text = el('span', 'task-text', task.text);
        for (const tag of task.tags || []) text.appendChild(el('span', 'task-tag', '#' + tag));
        row.appendChild(text);

        const doc = el('span', 'task-doc', shortName(task.display || task.doc || ''));
        doc.title = task.display || task.doc || '';
        doc.addEventListener('click', () => app.openDoc(task.doc));
        row.appendChild(doc);

        return row;
    }

    function threadCard(thread) {
        const card = el('div', 'comment-card comment-card--' + thread.status);
        const head = el('div', 'comment-card-head');
        head.appendChild(el('span', 'comment-author', thread.author));
        const chip = thread.status === 'open' && thread.replies.length ? 'replied' : thread.status;
        head.appendChild(el('span', 'status-chip status-chip--' + chip, chip));
        const where = el('span', 'task-doc', shortName(thread.display));
        where.title = thread.display;
        where.addEventListener('click', () => app.openDoc(thread.path, { drawer: 'comments' }));
        head.appendChild(where);
        head.appendChild(el('span', 'comment-time', global.UI.relativeTime(thread.created_at_iso)));
        card.appendChild(head);

        card.appendChild(el('div', 'comment-anchor', thread.excerpt));
        card.appendChild(el('div', 'comment-body', thread.body));

        if (thread.replies.length) {
            const last = thread.replies[thread.replies.length - 1];
            const replies = el('div', 'comment-replies');
            const item = el('div', 'comment-reply');
            const head2 = el('div', 'comment-reply-head');
            head2.appendChild(el('span', 'comment-reply-author', last.author));
            head2.appendChild(el('span', null, global.UI.relativeTime(last.created_at_iso)));
            item.appendChild(head2);
            item.appendChild(el('div', 'comment-reply-body', last.body));
            replies.appendChild(item);
            card.appendChild(replies);
        }

        if (thread.addressed_by_proposal) {
            const actions = el('div', 'comment-actions');
            const link = el('button', 'comment-link', 'Addressed by ' + thread.addressed_by_proposal + ' →');
            link.addEventListener('click', () => app.openReview(thread.addressed_by_proposal));
            actions.appendChild(link);
            card.appendChild(actions);
        }

        return card;
    }

    function shortName(display) {
        const parts = String(display || '').split('/');
        return parts.length <= 2 ? display : '…/' + parts.slice(-2).join('/');
    }

    const Today = {
        init(application) {
            app = application;
        },
        load,
    };

    global.Today = Today;
})(window);
