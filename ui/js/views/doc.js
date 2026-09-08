/* The document view: CodeMirror beside a live preview, with the timeline,
 * comments and validation drawer beneath.
 *
 * Selecting text raises a comment bubble — the find-bar pattern made
 * contextual. Anchored text is underlined in the theme's warning colour, and
 * clicking it opens the thread.
 */
(function (global) {
    'use strict';

    const { $, el, escapeHtml } = global.UI;

    let app = null;
    let editor = null;
    let previewTimer = null;
    let validateTimer = null;
    let caretTimer = null;
    let trackingSource = null;
    let editorSelection = { from: 0, to: 0, empty: true };

    const view = {
        path: null,
        doc: null,
        versions: [],
        comments: [],
        validation: null,
        dirty: false,
        // Two selected versions make a comparison.
        picked: [],
        drawer: 'timeline',
        previewOn: true,
        suppressChange: false,
    };

    // -----------------------------------------------------------------------
    // Editor
    // -----------------------------------------------------------------------

    function ensureEditor() {
        if (editor) return editor;
        editor = global.FolioCM.create($('editor-host'), {
            doc: '',
            placeholder: 'Open a document from the corpus, or create one with ' + global.Folio.shortcut('Mod+N') + '.',
            on: {
                change(text) {
                    if (view.suppressChange) return;
                    if (!view.dirty) {
                        view.dirty = true;
                        app.setDirty(true);
                    }
                    schedulePreview(text);
                    scheduleValidate();
                },
                selection(sel) {
                    app.setCursor(sel.line, sel.column);
                    editor.setGhostCaret(null);
                    trackingSource = 'editor';
                    editorSelection = { from: sel.from, to: sel.to, empty: sel.empty };
                    schedulePreviewLocation(editorSelection);
                    updateCommentBubble(sel);
                },
                anchorClick(commentId) {
                    openDrawer('comments');
                    highlightComment(commentId);
                },
            },
        });
        return editor;
    }

    function schedulePreview(text) {
        if (!view.previewOn) return;
        clearTimeout(previewTimer);
        previewTimer = setTimeout(() => {
            renderPreview(text);
        }, 120);
    }

    function renderPreview(text) {
        global.Markdown.render($('preview'), text);
        applyPreviewAnchors();
        if (trackingSource === 'editor') schedulePreviewLocation(editorSelection);
    }

    function schedulePreviewLocation(selection) {
        clearTimeout(caretTimer);
        caretTimer = setTimeout(() => {
            if (trackingSource !== 'editor') return;
            if (selection.empty) placePreviewCaret(selection.from);
            else placePreviewSelection(selection.from, selection.to);
        }, 60);
    }

    function clearPreviewCaret() {
        const caret = $('preview').querySelector('.preview-ghost-caret');
        if (!caret) return;
        const parent = caret.parentNode;
        caret.remove();
        parent.normalize();
    }

    function placePreviewCaret(offset) {
        clearPreviewCaret();
        global.Markdown.clearSelection($('preview'));
        const mapped = global.Markdown.mapOffset($('preview'), offset);
        if (!mapped) return;
        const caret = el('span', 'preview-ghost-caret');
        caret.setAttribute('aria-hidden', 'true');
        if (mapped.node) {
            const range = document.createRange();
            range.setStart(mapped.node, mapped.offset);
            range.collapse(true);
            range.insertNode(caret);
        } else {
            mapped.block.appendChild(caret);
        }
        caret.scrollIntoView({ block: 'nearest' });
    }

    function placePreviewSelection(from, to) {
        clearPreviewCaret();
        global.Markdown.applySelection($('preview'), from, to);
        const selections = $('preview').querySelectorAll('.preview-ghost-selection');
        selections[selections.length - 1]?.scrollIntoView({ block: 'nearest' });
    }

    function scheduleValidate() {
        clearTimeout(validateTimer);
        // Validation reads the file from disk, so it is only meaningful once
        // the buffer has been saved; running it on every keystroke would lie.
        validateTimer = setTimeout(() => {
            if (!view.dirty) refreshValidation();
        }, 600);
    }

    // -----------------------------------------------------------------------
    // Comment bubble
    // -----------------------------------------------------------------------

    let pendingSelection = null;

    function updateCommentBubble(sel) {
        const bubble = $('comment-bubble');
        if (!sel || sel.empty || !view.path || !sel.coords || !$('find-bar').classList.contains('hidden')) {
            bubble.classList.add('hidden');
            pendingSelection = null;
            return;
        }
        pendingSelection = { from: sel.from, to: sel.to, text: sel.text };
        const host = $('view-doc').getBoundingClientRect();

        // Measure before placing so the centred bubble stays within the view
        // even when the selection ends against an edge of the editor.
        bubble.style.visibility = 'hidden';
        bubble.classList.remove('hidden');
        const bounds = bubble.getBoundingClientRect();
        const margin = 8;
        const minLeft = bounds.width / 2 + margin;
        const maxLeft = host.width - bounds.width / 2 - margin;
        const minTop = bounds.height + margin;
        const maxTop = host.height - margin;
        bubble.style.left = Math.max(minLeft, Math.min(sel.coords.left - host.left, maxLeft)) + 'px';
        bubble.style.top = Math.max(minTop, Math.min(sel.coords.top - host.top - 6, maxTop)) + 'px';
        bubble.style.visibility = '';
    }

    function previewSelection() {
        const selection = global.getSelection();
        if (!selection || selection.rangeCount !== 1 || selection.isCollapsed) return null;
        if ($('preview')._folioSource !== editor.getValue()) return null;
        const range = selection.getRangeAt(0);
        const mapped = global.Markdown.mapRange($('preview'), range);
        if (!mapped) return null;
        const bounds = range.getBoundingClientRect();
        return {
            ...mapped,
            empty: false,
            coords: { left: bounds.left + bounds.width / 2, top: bounds.top },
        };
    }

    function handlePreviewSelection() {
        const selection = global.getSelection();
        if (!selection || !selection.anchorNode || !$('preview').contains(selection.anchorNode)) return;
        const mapped = previewSelection();
        updateCommentBubble(mapped);
        if (mapped) {
            clearPreviewCaret();
            editor.setGhostSelection(mapped.from, mapped.to);
            trackingSource = 'preview';
            clearTimeout(caretTimer);
        }
    }

    function handlePreviewClick(event) {
        const anchor = event.target.closest?.('[data-comment-id]');
        if (anchor) {
            openDrawer('comments');
            highlightComment(anchor.dataset.commentId);
            return;
        }
        const selection = global.getSelection();
        if (selection && !selection.isCollapsed && $('preview').contains(selection.anchorNode)) return;
        updateCommentBubble(null);
        const offset = global.Markdown.mapPoint($('preview'), event.clientX, event.clientY);
        if (offset === null) return;
        placePreviewCaret(offset);
        editor.setGhostCaret(offset);
        trackingSource = 'preview';
        clearTimeout(caretTimer);
    }

    async function startComment() {
        if (!pendingSelection || !view.path) return;
        if (view.dirty) {
            const go = await global.UI.confirm(
                'Save this document before commenting? A comment anchors to text that is on disk.',
                { title: 'Unsaved changes', okLabel: 'Save and comment' }
            );
            if (!go) return;
            await save();
        }
        $('comment-anchor-preview').textContent = pendingSelection.text;
        $('comment-body').value = '';
        global.UI.openDialog('comment-dialog');
    }

    async function confirmComment() {
        const body = $('comment-body').value.trim();
        if (!body) {
            global.UI.toast('A comment needs a body.', { type: 'warning' });
            return;
        }
        if (!pendingSelection) return;
        try {
            await global.Folio.call('create_comment', {
                path: view.path,
                selection_start: editor.toByteOffset(pendingSelection.from),
                selection_end: editor.toByteOffset(pendingSelection.to),
                body,
            });
            global.UI.closeDialog();
            $('comment-bubble').classList.add('hidden');
            await refreshComments();
            openDrawer('comments');
            global.UI.toast('Comment added. Connected agents will see it as an open work item.', {
                type: 'success',
            });
            app.refreshDocs();
        } catch (e) {
            global.UI.error(e, 'Could not add the comment');
        }
    }

    // -----------------------------------------------------------------------
    // Loading
    // -----------------------------------------------------------------------

    async function open(path, options) {
        const { keepScroll = false } = options || {};
        ensureEditor();

        if (view.dirty && view.path && view.path !== path) {
            const choice = await global.UI.confirm(
                'You have unsaved changes in ' + view.doc.display + '. Save them first?',
                { title: 'Unsaved changes', okLabel: 'Save', cancelLabel: 'Discard' }
            );
            if (choice) await save();
        }

        try {
            const doc = await global.Folio.call('read_doc', { path });
            view.path = doc.path;
            view.doc = doc;
            view.dirty = false;
            view.picked = [];
            view.comments = [];
            app.setDirty(false);

            view.suppressChange = true;
            editor.setValue(doc.content, { preserveCursor: keepScroll });
            editor.setEditable(doc.type !== 'asset');
            view.suppressChange = false;

            renderPreview(doc.content);
            app.setDocument(doc);

            await Promise.all([refreshTimeline(), refreshComments(), refreshValidation()]);
        } catch (e) {
            global.UI.error(e, 'Could not open the document');
        }
    }

    async function prepareToClose() {
        if (!view.dirty) return true;
        const saveFirst = await global.UI.confirm(
            'You have unsaved changes in ' + view.doc.display + '. Save them before closing?',
            { title: 'Unsaved changes', okLabel: 'Save', cancelLabel: 'Discard' }
        );
        if (saveFirst) return save();
        else {
            view.dirty = false;
            app.setDirty(false);
            return true;
        }
    }

    function clear() {
        clearTimeout(previewTimer);
        clearTimeout(validateTimer);
        view.path = null;
        view.doc = null;
        view.versions = [];
        view.comments = [];
        view.validation = null;
        view.dirty = false;
        view.picked = [];
        pendingSelection = null;

        view.suppressChange = true;
        editor.setValue('');
        editor.setEditable(false);
        view.suppressChange = false;
        global.Markdown.render($('preview'), '');
        $('drawer-timeline').innerHTML = '';
        $('drawer-comments').innerHTML = '';
        $('drawer-findings').innerHTML = '';
        $('comments-badge').hidden = true;
        $('findings-badge').hidden = true;
        $('findings-strip').classList.add('hidden');
        $('comment-bubble').classList.add('hidden');
        showFind(false);
    }

    async function refreshTimeline() {
        if (!view.path) return;
        const result = await global.Folio.tryCall('list_versions', { path: view.path, limit: 300 }, { versions: [] });
        view.versions = result.versions || [];
        renderTimeline();
        app.setDocumentFacts(view.versions.length);
    }

    async function refreshComments() {
        if (!view.path) return;
        const result = await global.Folio.tryCall('list_comments', { path: view.path }, { comments: [] });
        view.comments = result.comments || [];
        renderComments();
        applyAnchors();
        const open = view.comments.filter((c) => c.status === 'open' || c.status === 'outdated').length;
        const badge = $('comments-badge');
        badge.textContent = String(open);
        badge.hidden = open === 0;
    }

    async function refreshValidation() {
        if (!view.path) return;
        const report = await global.Folio.tryCall('validate_doc', { path: view.path }, null);
        view.validation = report;
        renderFindings();
        if (editor && report) {
            editor.setFindings(report.findings.filter((f) => f.line && !f.file));
        }
    }

    function applyAnchors() {
        if (!editor) return;
        const anchors = view.comments
            .filter((c) => c.anchor && c.status !== 'resolved')
            .map((c) => ({
                id: c.id,
                from: editor.fromByteOffset(c.anchor.offset),
                to: editor.fromByteOffset(c.anchor.end),
                outdated: c.status === 'outdated',
                title: c.author + ': ' + c.body.slice(0, 90),
            }));
        editor.setAnchors(anchors);
        applyPreviewAnchors();
    }

    function applyPreviewAnchors() {
        const preview = $('preview');
        if (!preview._folioSource) return;
        for (const anchor of preview.querySelectorAll('.preview-anchor, .preview-anchor-outdated')) {
            anchor.replaceWith(...anchor.childNodes);
        }
        if (view.dirty) return;
        const anchors = view.comments
            .filter((c) => c.anchor && c.status !== 'resolved')
            .map((c) => ({
                id: c.id,
                from: editor.fromByteOffset(c.anchor.offset),
                to: editor.fromByteOffset(c.anchor.end),
                outdated: c.status === 'outdated',
                title: c.author + ': ' + c.body.slice(0, 90),
            }));
        global.Markdown.applyAnchors(preview, anchors);
    }

    // -----------------------------------------------------------------------
    // Drawer: timeline
    // -----------------------------------------------------------------------

    function renderTimeline() {
        const host = $('drawer-timeline');
        host.innerHTML = '';
        if (!view.versions.length) {
            host.appendChild(el('div', 'drawer-empty', 'No versions recorded yet.'));
            return;
        }

        view.versions.forEach((snap, index) => {
            const row = el('button', 'timeline-row');
            row.type = 'button';
            if (index === 0) row.classList.add('current');
            if (view.picked.includes(snap.id)) row.classList.add('selected');

            const pick = el('span', 'timeline-pick');
            pick.textContent = view.picked.includes(snap.id) ? String(view.picked.indexOf(snap.id) + 1) : '';

            const badge = el('span', 'source-badge source-badge--' + snap.source, snap.source);

            const main = el('div', 'timeline-main');
            const author = el('span', 'timeline-author', snap.author);
            main.appendChild(author);
            if (snap.message) {
                main.appendChild(document.createTextNode(' '));
                main.appendChild(el('span', 'timeline-message', snap.message));
            }

            const time = el('span', 'timeline-time', global.UI.relativeTime(snap.created_at_iso));
            time.title = global.UI.absoluteTime(snap.created_at_iso) + '  ·  ' + snap.id;

            const delta = el('span', 'timeline-delta ' +
                (snap.size_delta > 0 ? 'timeline-delta--up' : snap.size_delta < 0 ? 'timeline-delta--down' : ''),
                global.UI.delta(snap.size_delta));

            row.appendChild(pick);
            row.appendChild(badge);
            row.appendChild(main);
            row.appendChild(delta);
            row.appendChild(time);

            row.addEventListener('click', () => togglePick(snap.id));
            host.appendChild(row);
        });

        $('drawer-compare').disabled = view.picked.length !== 2;
        $('drawer-restore').disabled = view.picked.length !== 1;
    }

    function togglePick(id) {
        const at = view.picked.indexOf(id);
        if (at >= 0) view.picked.splice(at, 1);
        else {
            view.picked.push(id);
            if (view.picked.length > 2) view.picked.shift();
        }
        renderTimeline();
    }

    async function compareSelected() {
        if (view.picked.length !== 2) return;
        // Timeline is newest-first, so the earlier pick in document order is
        // the one further down the list.
        const order = view.versions.map((v) => v.id);
        const [a, b] = view.picked.slice().sort((x, y) => order.indexOf(y) - order.indexOf(x));
        try {
            const result = await global.Folio.call('diff_versions', { path: view.path, from: a, to: b });
            showComparison(result);
        } catch (e) {
            global.UI.error(e, 'Could not compare those versions');
        }
    }

    function showComparison(result) {
        const body = $('validate-body');
        body.innerHTML = '';

        const head = el('div', 'proposal-head');
        head.innerHTML =
            '<div class="proposal-head-title">' +
            '<span class="proposal-path">' + escapeHtml(result.from.display) + '</span>' +
            '<span class="status-chip status-chip--open">compare</span>' +
            '</div>' +
            '<div class="proposal-meta">' +
            escapeHtml(result.from.source + ' by ' + result.from.author + ' · ' +
                global.UI.absoluteTime(result.from.created_at_iso)) +
            '<br>→ ' +
            escapeHtml(result.to.source + ' by ' + result.to.author + ' · ' +
                global.UI.absoluteTime(result.to.created_at_iso)) +
            '</div>';
        body.appendChild(head);

        const diffHost = el('div', 'diff');
        body.appendChild(diffHost);
        global.DiffView.render(diffHost, result.diff, { review: false });

        $('validate-summary').textContent = global.DiffView.summary(result.diff);
        $('validate-dialog').querySelector('h3').textContent = 'Compare versions';
        global.UI.openDialog('validate-dialog');
    }

    async function restoreSelected() {
        if (view.picked.length !== 1) return;
        const snap = view.versions.find((v) => v.id === view.picked[0]);
        if (!snap) return;
        const go = await global.UI.confirm(
            'Restore the version from ' + global.UI.absoluteTime(snap.created_at_iso) +
            ' (' + snap.source + ' by ' + snap.author + ')?\n\n' +
            'The restore is itself recorded as a version, so it can be undone.',
            { title: 'Restore version', okLabel: 'Restore' }
        );
        if (!go) return;
        try {
            await global.Folio.call('restore_version', { path: view.path, version_id: snap.id });
            view.picked = [];
            await open(view.path);
            global.UI.toast('Restored. The restore is itself a version.', { type: 'success' });
        } catch (e) {
            global.UI.error(e, 'Could not restore');
        }
    }

    // -----------------------------------------------------------------------
    // Drawer: comments
    // -----------------------------------------------------------------------

    function statusChip(comment) {
        if (comment.status === 'open' && comment.replies.length) return 'replied';
        return comment.status;
    }

    function renderComments() {
        const host = $('drawer-comments');
        host.innerHTML = '';
        if (!view.comments.length) {
            const empty = el('div', 'drawer-empty');
            empty.innerHTML = 'No comments on this document.<br>' +
                'Select any passage and press <strong>Comment</strong> to leave a note ' +
                'every connected agent can read and address.';
            host.appendChild(empty);
            return;
        }

        for (const comment of view.comments) {
            host.appendChild(commentCard(comment));
        }
    }

    function commentCard(comment) {
        const card = el('div', 'comment-card comment-card--' + comment.status);
        card.dataset.commentId = comment.id;

        const head = el('div', 'comment-card-head');
        head.appendChild(el('span', 'comment-author', comment.author));
        const chip = statusChip(comment);
        head.appendChild(el('span', 'status-chip status-chip--' + chip, chip));
        const time = el('span', 'comment-time', global.UI.relativeTime(comment.created_at_iso));
        time.title = global.UI.absoluteTime(comment.created_at_iso) + '  ·  ' + comment.id;
        head.appendChild(time);
        card.appendChild(head);

        const anchor = el('div', 'comment-anchor', comment.excerpt);
        if (comment.anchor) {
            anchor.title = 'Jump to the anchored text';
            anchor.addEventListener('click', () => editor.scrollTo(editor.fromByteOffset(comment.anchor.offset)));
        } else if (comment.status === 'outdated') {
            anchor.title = 'The anchored text has changed; this thread is kept, not dropped.';
        } else if (comment.status === 'orphaned') {
            anchor.title = 'The document is gone; this thread is retained.';
        }
        card.appendChild(anchor);

        card.appendChild(el('div', 'comment-body', comment.body));

        if (comment.replies.length) {
            const replies = el('div', 'comment-replies');
            for (const reply of comment.replies) {
                const item = el('div', 'comment-reply');
                const head2 = el('div', 'comment-reply-head');
                head2.appendChild(el('span', 'comment-reply-author', reply.author));
                head2.appendChild(el('span', null, global.UI.relativeTime(reply.created_at_iso)));
                item.appendChild(head2);
                item.appendChild(el('div', 'comment-reply-body', reply.body));
                replies.appendChild(item);
            }
            card.appendChild(replies);
        }

        const actions = el('div', 'comment-actions');
        if (comment.addressed_by_proposal) {
            const link = el('button', 'comment-link', 'Addressed by ' + comment.addressed_by_proposal + ' →');
            link.addEventListener('click', () => app.openReview(comment.addressed_by_proposal));
            actions.appendChild(link);
        }
        if (comment.status === 'resolved') {
            const reopen = el('button', 'comment-link', 'Reopen');
            reopen.addEventListener('click', () => decideComment('reopen_comment', comment.id));
            actions.appendChild(reopen);
        } else {
            const resolve = el('button', 'comment-link', 'Resolve');
            resolve.addEventListener('click', () => decideComment('resolve_comment', comment.id));
            actions.appendChild(resolve);
        }
        const replyBtn = el('button', 'comment-link', 'Reply');
        replyBtn.addEventListener('click', () => toggleReplyForm(card, comment.id));
        actions.appendChild(replyBtn);

        const remove = el('button', 'comment-link', 'Delete');
        remove.style.color = 'var(--error-color)';
        remove.addEventListener('click', async () => {
            const go = await global.UI.confirm('Delete this thread and its replies?', {
                title: 'Delete comment', okLabel: 'Delete', danger: true,
            });
            if (go) decideComment('delete_comment', comment.id);
        });
        actions.appendChild(remove);

        card.appendChild(actions);
        return card;
    }

    function toggleReplyForm(card, commentId) {
        const existing = card.querySelector('.comment-reply-form');
        if (existing) {
            existing.remove();
            return;
        }
        const form = el('div', 'comment-reply-form');
        const input = document.createElement('textarea');
        input.rows = 2;
        input.placeholder = 'Reply…';
        const send = el('button', 'btn-primary', 'Send');
        send.style.padding = '4px 12px';
        send.addEventListener('click', async () => {
            const body = input.value.trim();
            if (!body) return;
            try {
                await global.Folio.call('reply_comment', { comment_id: commentId, body });
                await refreshComments();
            } catch (e) {
                global.UI.error(e, 'Could not post the reply');
            }
        });
        input.addEventListener('keydown', (event) => {
            if (event.key === 'Enter' && (event.ctrlKey || event.metaKey)) send.click();
        });
        form.appendChild(input);
        form.appendChild(send);
        card.appendChild(form);
        input.focus();
    }

    async function decideComment(op, commentId) {
        try {
            await global.Folio.call(op, { comment_id: commentId });
            await refreshComments();
            app.refreshDocs();
        } catch (e) {
            global.UI.error(e, 'Could not update the thread');
        }
    }

    function highlightComment(commentId) {
        const card = $('drawer-comments').querySelector('[data-comment-id="' + commentId + '"]');
        if (!card) return;
        card.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
        card.style.transition = 'box-shadow 0.4s ease';
        card.style.boxShadow = '0 0 0 2px var(--accent-primary)';
        setTimeout(() => { card.style.boxShadow = ''; }, 1200);
    }

    // -----------------------------------------------------------------------
    // Drawer: validation findings
    // -----------------------------------------------------------------------

    function renderFindings() {
        const host = $('drawer-findings');
        const strip = $('findings-strip');
        host.innerHTML = '';

        const report = view.validation;
        if (!report) {
            strip.classList.add('hidden');
            host.appendChild(el('div', 'drawer-empty', 'No validation report.'));
            $('findings-badge').hidden = true;
            return;
        }

        const badge = $('findings-badge');
        const problems = report.errors + report.warnings;
        badge.textContent = String(problems);
        badge.hidden = problems === 0;

        strip.classList.remove('hidden');
        strip.className = 'findings-strip ' +
            (report.errors ? 'findings-strip--error' : report.warnings ? 'findings-strip--warning' : 'findings-strip--ok');
        const headline = report.errors
            ? report.errors + ' error' + (report.errors === 1 ? '' : 's')
            : report.warnings
                ? report.warnings + ' warning' + (report.warnings === 1 ? '' : 's')
                : 'Valid ' + global.UI.typeLabel(report.type).toLowerCase();
        const first = report.findings.find((f) => f.severity !== 'info');
        strip.innerHTML =
            '<strong>' + escapeHtml(headline) + '</strong>' +
            '<span class="findings-strip-detail">' + escapeHtml(first ? first.message : '') + '</span>';
        strip.onclick = () => openDrawer('findings');

        if (!report.findings.length) {
            host.appendChild(el('div', 'drawer-empty', 'Nothing to report.'));
            return;
        }

        for (const finding of report.findings) {
            const row = el('button', 'finding-row');
            row.type = 'button';
            const icon = el('span', 'finding-icon finding-icon--' + finding.severity,
                finding.severity === 'error' ? '●' : finding.severity === 'warning' ? '▲' : '○');
            const body = el('div');
            body.appendChild(el('span', 'finding-message', finding.message));
            const meta = [finding.rule];
            if (finding.file) meta.push(finding.file);
            if (finding.line) meta.push('line ' + finding.line);
            if (finding.hint) meta.push(finding.hint);
            body.appendChild(el('span', 'finding-meta', meta.join('  ·  ')));
            row.appendChild(icon);
            row.appendChild(body);
            if (finding.line && !finding.file) {
                row.addEventListener('click', () => editor.scrollTo(editor.lineOffset(finding.line)));
            }
            host.appendChild(row);
        }
    }

    // -----------------------------------------------------------------------
    // Drawer plumbing
    // -----------------------------------------------------------------------

    function openDrawer(which, options) {
        const { remember = true } = options || {};
        view.drawer = which;
        if (remember) {
            global.Folio.tryCall('ui_state_set', { key: 'drawer', value: which }, null);
        }
        $('drawer').classList.remove('collapsed');
        for (const tab of document.querySelectorAll('.drawer-tab')) {
            tab.classList.toggle('active', tab.dataset.drawer === which);
        }
        for (const id of ['timeline', 'comments', 'findings']) {
            $('drawer-' + id).classList.toggle('hidden', id !== which);
        }
        const timelineTools = view.drawer === 'timeline';
        $('drawer-compare').style.display = timelineTools ? '' : 'none';
        $('drawer-restore').style.display = timelineTools ? '' : 'none';
    }

    function toggleDrawer() {
        $('drawer').classList.toggle('collapsed');
    }

    // -----------------------------------------------------------------------
    // Actions
    // -----------------------------------------------------------------------

    async function save() {
        if (!view.path) return false;
        try {
            const result = await global.Folio.call('save_doc', {
                path: view.path,
                content: editor.getValue(),
            });
            view.dirty = false;
            app.setDirty(false);
            if (result.created) {
                global.UI.status('Saved · version ' + result.snapshot.id);
            } else {
                global.UI.status('No change — no new version');
            }
            await Promise.all([refreshTimeline(), refreshValidation(), refreshComments()]);
            app.refreshDocs();
            return true;
        } catch (e) {
            global.UI.error(e, 'Could not save');
            return false;
        }
    }

    async function checkpoint() {
        if (!view.path) return;
        const message = await global.UI.prompt('What does this checkpoint mark?', {
            title: 'Checkpoint',
            placeholder: 'before restructuring commands',
            okLabel: 'Checkpoint',
        });
        if (message === null) return;
        if (view.dirty) await save();
        try {
            const result = await global.Folio.call('checkpoint', { path: view.path, message: message || 'checkpoint' });
            await refreshTimeline();
            global.UI.toast('Checkpoint ' + result.snapshot.id + ' recorded.', { type: 'success' });
        } catch (e) {
            global.UI.error(e, 'Could not record the checkpoint');
        }
    }

    async function exportHistory() {
        if (!view.path) return;
        try {
            const result = await global.Folio.call('export_history', { path: view.path });
            $('export-output').textContent = result.patch_series;
            global.UI.openDialog('export-dialog');
        } catch (e) {
            global.UI.error(e, 'Could not export the history');
        }
    }

    function setPreview(on) {
        view.previewOn = on;
        $('doc-panes').classList.toggle('preview-hidden', !on);
        if (on && editor) renderPreview(editor.getValue());
        try { localStorage.setItem('folio.preview', on ? '1' : '0'); } catch (e) { /* ignore */ }
    }

    // -----------------------------------------------------------------------
    // Find bar (in-document)
    // -----------------------------------------------------------------------

    let findMatches = [];
    let findIndex = -1;
    let findLength = 0;

    function runFind(query) {
        findMatches = [];
        findIndex = -1;
        findLength = query.length;
        if (!query || !editor) {
            $('find-count').textContent = '';
            return;
        }
        const text = editor.getValue().toLowerCase();
        const needle = query.toLowerCase();
        let at = text.indexOf(needle);
        while (at !== -1 && findMatches.length < 5000) {
            findMatches.push(at);
            at = text.indexOf(needle, at + Math.max(1, needle.length));
        }
        $('find-count').textContent = findMatches.length ? '1 of ' + findMatches.length : 'no matches';
        if (findMatches.length) stepFind(0);
    }

    function stepFind(direction) {
        if (!findMatches.length) return;
        findIndex = (findIndex + direction + findMatches.length) % findMatches.length;
        if (findIndex < 0) findIndex = 0;
        editor.scrollTo(findMatches[findIndex], { focus: false, selectionLength: findLength });
        $('find-count').textContent = (findIndex + 1) + ' of ' + findMatches.length;
    }

    function showFind(on) {
        const bar = $('find-bar');
        bar.classList.toggle('hidden', !on);
        if (on) {
            $('find-input').focus();
            $('find-input').select();
        } else if (editor) {
            editor.focus();
        }
    }

    // -----------------------------------------------------------------------

    const DocView = {
        init(application) {
            app = application;
            ensureEditor();

            $('comment-bubble-btn').addEventListener('click', startComment);
            $('comment-confirm').addEventListener('click', confirmComment);
            $('preview').addEventListener('pointerdown', () => {
                clearPreviewCaret();
                global.Markdown.clearSelection($('preview'));
                editor.setGhostCaret(null);
                trackingSource = 'preview';
                clearTimeout(caretTimer);
            });
            $('preview').addEventListener('mouseup', () => setTimeout(handlePreviewSelection));
            $('preview').addEventListener('click', handlePreviewClick);

            for (const tab of document.querySelectorAll('.drawer-tab')) {
                tab.addEventListener('click', () => openDrawer(tab.dataset.drawer));
            }
            $('drawer-collapse').addEventListener('click', toggleDrawer);
            $('drawer-compare').addEventListener('click', compareSelected);
            $('drawer-restore').addEventListener('click', restoreSelected);

            $('find-input').addEventListener('input', (event) => runFind(event.target.value));
            $('find-input').addEventListener('keydown', (event) => {
                if (event.key === 'Enter') stepFind(event.shiftKey ? -1 : 1);
                if (event.key === 'Escape') showFind(false);
            });
            $('find-next').addEventListener('click', () => stepFind(1));
            $('find-prev').addEventListener('click', () => stepFind(-1));
            $('find-close').addEventListener('click', () => showFind(false));

            global.UI.initSplitter('pane-splitter', 'editor-pane', 'x', {
                min: 260, storageKey: 'folio.editorWidth',
            });
            global.UI.initSplitter('drawer-splitter', 'drawer', 'y', {
                min: 90, storageKey: 'folio.drawerHeight',
            });

            let previewPreference = true;
            try { previewPreference = localStorage.getItem('folio.preview') !== '0'; } catch (e) { /* ignore */ }
            setPreview(previewPreference);
            openDrawer('timeline', { remember: false });
            global.Folio.tryCall('ui_state_get', { key: 'drawer' }, null).then((stored) => {
                const which = stored && stored.value;
                if (which === 'comments' || which === 'findings') openDrawer(which, { remember: false });
            });
        },

        open,
        prepareToClose,
        clear,
        save,
        checkpoint,
        exportHistory,
        setPreview,
        togglePreview: () => setPreview(!view.previewOn),
        previewOn: () => view.previewOn,
        toggleDrawer,
        openDrawer,
        showFind,
        isDirty: () => view.dirty,
        path: () => view.path,
        current: () => view.doc,
        refresh: async () => {
            if (view.path) await Promise.all([refreshTimeline(), refreshComments(), refreshValidation()]);
        },
        /** Re-read from disk after an external change, unless the user is mid-edit. */
        async reloadIfClean() {
            if (!view.path || view.dirty) return false;
            await open(view.path, { keepScroll: true });
            return true;
        },
        resolveFocusedComment() {
            const openThread = view.comments.find((c) => c.status === 'open' || c.status === 'outdated');
            if (!openThread) {
                global.UI.toast('No open comment on this document.', { type: 'info' });
                return;
            }
            decideComment('resolve_comment', openThread.id);
        },
        editor: () => editor,
    };

    global.DocView = DocView;
})(window);
