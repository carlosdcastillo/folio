/* The shell: state, view switching, menus, shortcuts, the status bar, and the
 * subscription to core events.
 *
 * Toasts for events, dialogs for decisions. A proposal arriving is a toast; a
 * comment reply from an agent is a toast; accepting a proposal is a dialog.
 */
(function (global) {
    'use strict';

    const { $, el } = global.UI;

    const state = {
        boot: {},
        roots: [],
        docs: [],
        proposals: [],
        filter: '',
        currentPath: null,
        currentDoc: null,
        dirty: false,
        openDocs: [],
        view: 'doc',
        theme: 'dark',
        history: [],
        historyIndex: -1,
    };

    // -----------------------------------------------------------------------
    // Theme
    // -----------------------------------------------------------------------

    /**
     * `persist` is false when restoring a remembered theme rather than
     * choosing one — writing it back would clobber the value being restored.
     */
    function setTheme(theme, options) {
        const { persist = true } = options || {};
        state.theme = theme === 'light' ? 'light' : 'dark';
        document.documentElement.setAttribute('data-theme', state.theme);
        document.querySelector('meta[name="color-scheme"]').content = state.theme;
        // highlight.js ships one stylesheet per theme; swap it with the app's.
        $('hljs-theme').href = state.theme === 'light' ? 'lib/github.min.css' : 'lib/nord.min.css';
        try { localStorage.setItem('folio.theme', state.theme); } catch (e) { /* ignore */ }
        if (persist) {
            // The shell reads this to paint the window itself in the right
            // colour next launch, before the webview has drawn anything.
            global.Folio.tryCall('ui_state_set', { key: 'theme', value: state.theme }, null);
        }
    }

    // -----------------------------------------------------------------------
    // Views
    // -----------------------------------------------------------------------

    async function showView(name, options) {
        const { remember = true } = options || {};
        state.view = name;
        // Folio is meant to be open every morning; it should come back to
        // whatever you were last looking at rather than always to a document.
        if (remember) {
            global.Folio.tryCall('ui_state_set', { key: 'view', value: name }, null);
        }
        for (const section of document.querySelectorAll('.view')) {
            section.classList.toggle('active', section.id === 'view-' + name);
        }
        for (const button of document.querySelectorAll('.view-btn')) {
            button.classList.toggle('active', button.dataset.view === name);
        }
        if (name === 'review') await global.Review.load();
        if (name === 'today') await global.Today.load();
    }

    // -----------------------------------------------------------------------
    // Data
    // -----------------------------------------------------------------------

    async function refreshRoots() {
        const result = await global.Folio.tryCall('list_roots', {}, { roots: [] });
        state.roots = result.roots || [];
        await refreshDocs();
    }

    async function refreshDocs() {
        const [docs, proposals] = await Promise.all([
            global.Folio.tryCall('list_docs', {}, { docs: [] }),
            global.Folio.tryCall('list_proposals', { status: 'pending' }, { proposals: [] }),
        ]);
        state.docs = docs.docs || [];
        state.proposals = proposals.proposals || [];
        global.Sidebar.render(state);
        updateStatusBar();
    }

    async function openDoc(path, options) {
        const { line = null, drawer = null, record = true } = options || {};
        if (!path) return;
        await showView('doc');
        await global.DocView.open(path);
        if (drawer) global.DocView.openDrawer(drawer);
        if (line) {
            const editor = global.DocView.editor();
            if (editor) editor.scrollTo(editor.lineOffset(line));
        }
        if (record) pushHistory(path);
        global.Folio.tryCall('ui_state_set', { key: 'lastDoc', value: path }, null);
        global.Sidebar.render(state);
    }

    function pushHistory(path) {
        if (state.history[state.historyIndex] === path) return;
        state.history = state.history.slice(0, state.historyIndex + 1);
        state.history.push(path);
        state.historyIndex = state.history.length - 1;
        updateNavButtons();
    }

    function updateNavButtons() {
        $('nav-back').disabled = state.historyIndex <= 0;
        $('nav-forward').disabled = state.historyIndex >= state.history.length - 1;
    }

    async function navigate(delta) {
        const next = state.historyIndex + delta;
        if (next < 0 || next >= state.history.length) return;
        state.historyIndex = next;
        updateNavButtons();
        await openDoc(state.history[next], { record: false });
    }

    // -----------------------------------------------------------------------
    // Chrome
    // -----------------------------------------------------------------------

    function setDocument(doc) {
        state.currentPath = doc.path;
        state.currentDoc = doc;

        const openIndex = state.openDocs.findIndex((open) => open.path === doc.path);
        if (openIndex === -1) state.openDocs.push(doc);
        else state.openDocs[openIndex] = doc;
        renderOpenFiles();

        $('doc-title').textContent = doc.display;
        $('doc-title').title = doc.path;
        const chip = $('doc-type-chip');
        chip.hidden = false;
        chip.className = 'type-chip type-chip--' + doc.type;
        chip.textContent = global.UI.typeLabel(doc.type);

        global.Folio.window.setTitle('Folio — ' + doc.display);
        $('window-title').textContent = 'Folio';
        const suffix = el('span', 'window-title-doc', '  —  ' + doc.display);
        $('window-title').appendChild(suffix);

        for (const action of ['save', 'close-tab', 'checkpoint', 'export-history', 'find', 'render-prompt', 'resolve-comment']) {
            global.UI.setMenuEnabled(action, true);
        }
        setDocumentFacts(doc.versions, doc.policy);
    }

    function setDocumentFacts(versionCount, policy) {
        const doc = state.currentDoc;
        if (!doc) return;
        const parts = [];
        parts.push('v' + (versionCount != null ? versionCount : doc.versions));
        const effective = policy || doc.policy;
        if (effective === 'direct') parts.push('direct writes');
        else parts.push('agent edits proposed');
        if (!doc.exists) parts.push('missing on disk');
        $('doc-facts').textContent = parts.join(' · ');
    }

    function setDirty(dirty) {
        state.dirty = dirty;
        $('doc-title').classList.toggle('dirty', dirty);
        renderOpenFiles();
    }

    async function closeDoc(path) {
        const index = state.openDocs.findIndex((doc) => doc.path === path);
        if (index === -1) return;

        const isActive = path === state.currentPath;
        if (isActive && !(await global.DocView.prepareToClose())) return;
        state.openDocs.splice(index, 1);

        if (!isActive) {
            renderOpenFiles();
            return;
        }

        const next = state.openDocs[Math.min(index, state.openDocs.length - 1)];
        if (next) {
            await openDoc(next.path);
            return;
        }

        state.currentPath = null;
        state.currentDoc = null;
        state.dirty = false;
        global.DocView.clear();
        renderOpenFiles();
        $('doc-title').textContent = 'No document open';
        $('doc-title').title = '';
        $('doc-title').classList.remove('dirty');
        $('doc-type-chip').hidden = true;
        $('doc-facts').textContent = '';
        $('window-title').textContent = 'Folio';
        global.Folio.window.setTitle('Folio');
        for (const action of ['save', 'close-tab', 'checkpoint', 'export-history', 'find', 'render-prompt', 'resolve-comment']) {
            global.UI.setMenuEnabled(action, false);
        }
        global.Sidebar.render(state);
    }

    function renderOpenFiles() {
        const host = $('open-files');
        host.innerHTML = '';

        for (const doc of state.openDocs) {
            const tab = el('div', 'open-file');
            if (doc.path === state.currentPath) {
                tab.classList.add('active');
                if (state.dirty) tab.classList.add('dirty');
            }

            const select = el('button', 'open-file-select');
            select.type = 'button';
            select.title = doc.path;
            select.setAttribute('aria-label', 'Open ' + doc.display);
            if (doc.path === state.currentPath) select.setAttribute('aria-current', 'page');
            const icon = el('span', 'open-file-icon doc-icon--' + doc.type, global.UI.typeIcon(doc.type));
            icon.setAttribute('aria-hidden', 'true');
            select.appendChild(icon);
            select.appendChild(el('span', 'open-file-name', doc.display));
            select.addEventListener('click', () => openDoc(doc.path));
            tab.appendChild(select);

            const close = el('button', 'open-file-close', '×');
            close.type = 'button';
            close.title = 'Close ' + doc.display;
            close.setAttribute('aria-label', 'Close ' + doc.display);
            close.addEventListener('click', () => closeDoc(doc.path));
            tab.appendChild(close);
            host.appendChild(tab);
        }

        const active = host.querySelector('.open-file.active');
        if (active) active.scrollIntoView({ block: 'nearest', inline: 'nearest' });
    }

    function cycleOpenDocs(delta) {
        if (state.openDocs.length < 2) return;
        const current = state.openDocs.findIndex((doc) => doc.path === state.currentPath);
        const next = (current + delta + state.openDocs.length) % state.openDocs.length;
        openDoc(state.openDocs[next].path);
    }

    function setCursor(line, column) {
        $('status-cursor').textContent = 'Ln ' + line + ', Col ' + column;
    }

    async function updateStatusBar() {
        const status = await global.Folio.tryCall('status', {}, null);
        if (!status) return;

        const watcher = $('status-watcher');
        const watching = status.watching || 0;
        watcher.textContent = 'watcher ●';
        watcher.className = 'status-connection ' +
            (watching ? 'status-connection--connected' : 'status-connection--disconnected');
        watcher.title = watching
            ? 'Watching ' + watching + ' root' + (watching === 1 ? '' : 's') + ' for external changes'
            : 'Not watching any root';

        const clients = status.clients || [];
        $('status-mcp').textContent = 'MCP: ' + (clients.length ? clients.map((c) => c.name).join(', ') : 'none');
        $('status-mcp').title = clients.length
            ? clients.map((c) => c.name + ' (' + c.mode + ')').join('\n')
            : 'No MCP client connected. Configure one with: folio mcp';

        const pending = status.pending_proposals || 0;

        // Another process can be writing to the same store — a bridge that
        // went headless, or a second window. Those writes emit no event into
        // this process, so notice the change and reload rather than showing a
        // stale tree until something else happens to refresh it.
        if (updateStatusBar.lastPending != null && updateStatusBar.lastPending !== pending) {
            updateStatusBar.lastPending = pending;
            refreshDocs();
            if (state.view === 'review') global.Review.load();
        } else {
            updateStatusBar.lastPending = pending;
        }

        const proposals = $('status-proposals');
        proposals.textContent = 'proposals: ' + pending;
        proposals.classList.toggle('has-pending', pending > 0);

        $('status-store').textContent = global.UI.bytes(status.store_bytes || 0) + ' store';


        const badge = $('review-badge');
        badge.textContent = String(pending);
        badge.hidden = pending === 0;

        if (status.cloud_sync_warning && !updateStatusBar.warned) {
            updateStatusBar.warned = true;
            global.UI.toast(
                'The store sits in a cloud-synced folder (' + status.cloud_sync_warning + '). ' +
                'SQLite and file sync corrupt each other.',
                { type: 'error', timeout: 0 }
            );
        }
    }

    // -----------------------------------------------------------------------
    // Menu and shortcuts
    // -----------------------------------------------------------------------

    const actions = {
        'new-doc': () => global.Prefs.openNewDoc(),
        'add-root': () => global.Prefs.openAddRoot(),
        save: () => global.DocView.save(),
        'close-tab': () => closeDoc(state.currentPath),
        checkpoint: () => global.DocView.checkpoint(),
        'export-history': () => global.DocView.exportHistory(),
        preferences: () => global.Prefs.open(),
        exit: () => global.Folio.window.close(),

        undo: () => document.execCommand('undo'),
        redo: () => document.execCommand('redo'),
        find: () => global.DocView.showFind(true),
        'find-in-corpus': () => global.Prefs.openSearch(),

        'toggle-preview': () => global.DocView.togglePreview(),
        timeline: () => {
            showView('doc');
            global.DocView.openDrawer('timeline');
        },
        today: () => showView('today'),
        'toggle-theme': () => setTheme(state.theme === 'dark' ? 'light' : 'dark'),

        review: () => showView('review'),
        'accept-all': async () => {
            await showView('review');
            global.Review.acceptEveryPending();
        },
        'reject-all': async () => {
            await showView('review');
            global.Review.rejectEveryPending();
        },
        comments: () => {
            showView('doc');
            global.DocView.openDrawer('comments');
        },
        'resolve-comment': () => global.DocView.resolveFocusedComment(),

        'mcp-status': () => global.Prefs.openMcp(),
        'validate-all': () => global.Prefs.validateAll(),
        'render-prompt': () => global.Prefs.openRender(),
        reindex: async () => {
            global.UI.loading(true, 'Re-indexing the corpus…');
            const result = await global.Folio.tryCall('index_all', {}, { indexed: 0 });
            global.UI.loading(false);
            await refreshDocs();
            global.UI.toast(result.indexed + ' new version(s) recorded.', { type: 'success' });
        },

        about: () => {
            $('about-body').innerHTML =
                '<div class="pref-facts">' +
                '<div><strong>Folio</strong> ' + global.UI.escapeHtml(state.boot.version || '') + '</div>' +
                '<div>Your agents’ markdown, under your control.</div>' +
                '<div style="margin-top:10px">The versioned, reviewable home for the markdown corpus a ' +
                'developer actually has. Files on disk stay plain markdown; the history, the review queue and ' +
                'the comment threads live in Folio’s private store.</div>' +
                '<div style="margin-top:10px"><strong>Store</strong> ' +
                global.UI.escapeHtml(state.boot.store_dir || '') + '</div>' +
                '</div>';
            global.UI.openDialog('about-dialog');
        },
    };

    function runAction(name) {
        const handler = actions[name];
        if (handler) handler();
    }

    function initShortcuts() {
        document.addEventListener('keydown', (event) => {
            const mod = event.ctrlKey || event.metaKey;
            if (!mod) {
                if (event.key === 'Escape') global.DocView.showFind(false);
                return;
            }

            const key = event.key.toLowerCase();
            const shift = event.shiftKey;

            if (key === 'tab') {
                event.preventDefault();
                cycleOpenDocs(shift ? -1 : 1);
                return;
            }

            if (key === 'enter') {
                event.preventDefault();
                runAction('resolve-comment');
                return;
            }

            const table = {
                n: 'new-doc',
                s: 'save',
                w: 'close-tab',
                k: 'checkpoint',
                ',': 'preferences',
                f: shift ? 'find-in-corpus' : 'find',
                p: 'toggle-preview',
                t: 'timeline',
                1: 'today',
                r: 'review',
                q: 'exit',
                d: shift ? 'toggle-theme' : null,
                c: shift ? 'comments' : null,
                v: shift ? 'validate-all' : null,
            };

            const action = table[key];
            if (!action) return;
            // Let the editor keep Ctrl+C / Ctrl+V; only claim the shifted forms.
            if ((key === 'c' || key === 'v' || key === 'd') && !shift) return;

            event.preventDefault();
            runAction(action);
        });
    }

    // -----------------------------------------------------------------------
    // Core events
    // -----------------------------------------------------------------------

    function initEvents() {
        global.Folio.on('proposal-arrived', async (event) => {
            const proposal = event.proposal;
            global.UI.toast(
                proposal.client + ' proposed changes to ' + shortName(proposal.display) + '.',
                {
                    type: 'info',
                    hint: proposal.message || '',
                    action: { label: 'Review', run: () => openReview(proposal.id) },
                }
            );
            await refreshDocs();
            if (state.view === 'review') await global.Review.load();
        });

        global.Folio.on('proposal-decided', async () => {
            await refreshDocs();
            if (state.view === 'review') await global.Review.load();
        });

        global.Folio.on('snapshot-created', async (event) => {
            const snapshot = event.snapshot;
            await refreshDocs();
            if (state.currentPath === snapshot.path) {
                // An external change to the file on screen: pick it up unless
                // the user is mid-edit, in which case say so and stay put.
                if (global.DocView.isDirty()) {
                    global.UI.toast(
                        shortName(snapshot.display) + ' changed on disk while you were editing.',
                        { type: 'warning', hint: 'Your buffer is untouched. Saving will create a new version on top.' }
                    );
                } else {
                    await global.DocView.reloadIfClean();
                }
            }
            if (state.view === 'today') await global.Today.load();
        });

        global.Folio.on('comment-activity', async (event) => {
            const comment = event.comment;
            if (event.kind === 'replied') {
                const last = comment.replies[comment.replies.length - 1];
                if (last && last.author !== 'you') {
                    global.UI.toast(last.author + ' replied on ' + shortName(comment.display) + '.', {
                        type: 'info',
                        hint: last.body.slice(0, 120),
                        action: { label: 'Open', run: () => openDoc(comment.path, { drawer: 'comments' }) },
                    });
                }
            }
            await refreshDocs();
            if (state.currentPath === comment.path) await global.DocView.refresh();
            if (state.view === 'today') await global.Today.load();
        });

        global.Folio.on('watcher-status', (event) => {
            updateStatusBar();
            if (!event.healthy && event.message) {
                global.UI.toast('Watcher: ' + event.message, { type: 'error' });
            }
        });

        global.Folio.on('clients-changed', () => updateStatusBar());
        global.Folio.on('corpus-changed', () => refreshRoots());

        global.Folio.on('doc-removed', async (event) => {
            global.UI.toast(shortName(event.display) + ' was deleted on disk. Its history is kept.', {
                type: 'warning',
            });
            await refreshDocs();
        });
    }

    function shortName(display) {
        const parts = String(display || '').split('/');
        return parts.length <= 2 ? display : '…/' + parts.slice(-2).join('/');
    }

    // -----------------------------------------------------------------------
    // Boot
    // -----------------------------------------------------------------------

    async function openReview(proposalId) {
        await showView('review');
        if (proposalId) await global.Review.select(proposalId);
    }


    const App = {
        state: () => state,
        theme: () => state.theme,
        setTheme,
        setDocument,
        setDocumentFacts,
        setDirty,
        setCursor,
        setProposals(list) {
            state.proposals = list;
        },
        refreshDocs,
        refreshRoots,
        openDoc,
        openReview,
        showView,
    };

    // A frontend error that only reaches the console is an error nobody sees.
    window.addEventListener('error', (event) => {
        global.UI.toast('Something in the interface failed: ' + (event.message || 'unknown error'), {
            type: 'error',
            hint: (event.filename || '').split('/').pop() + ':' + event.lineno,
            timeout: 0,
        });
    });
    window.addEventListener('unhandledrejection', (event) => {
        const reason = event.reason;
        // A rejected core call already reported itself; this is for the rest.
        if (reason && reason.name === 'FolioError') return;
        global.UI.toast('Something in the interface failed: ' +
            ((reason && reason.message) || String(reason)), { type: 'error', timeout: 0 });
    });

    document.addEventListener('DOMContentLoaded', async () => {
        global.Folio.applyPlatform(document.documentElement.dataset.platform);
        setTheme((function () {
            try { return localStorage.getItem('folio.theme') || 'dark'; } catch (e) { return 'dark'; }
        })(), { persist: false });

        global.UI.initMenus(runAction);
        initShortcuts();
        initEvents();

        for (const action of ['save', 'close-tab', 'checkpoint', 'export-history', 'render-prompt', 'resolve-comment']) {
            global.UI.setMenuEnabled(action, false);
        }

        $('window-minimize').addEventListener('click', () => global.Folio.window.minimize());
        $('window-maximize').addEventListener('click', () => global.Folio.window.toggleMaximize());
        $('window-close').addEventListener('click', () => global.Folio.window.close());

        $('nav-back').addEventListener('click', () => navigate(-1));
        $('nav-forward').addEventListener('click', () => navigate(1));
        $('toolbar-new-doc').addEventListener('click', () => global.Prefs.openNewDoc());
        for (const button of document.querySelectorAll('.view-btn')) {
            button.addEventListener('click', () => showView(button.dataset.view));
        }

        global.Sidebar.init({
            onOpen: (path) => openDoc(path),
            onAddRoot: () => global.Prefs.openAddRoot(),
            onReview: () => showView('review'),
            onFilter: (value) => {
                state.filter = value;
                global.Sidebar.render(state);
            },
        });
        global.DocView.init(App);
        global.Review.init(App);
        global.Today.init(App);
        global.Prefs.init(App);

        let rememberedView = null;
        try {
            state.boot = await global.Folio.boot();
            const stored = await global.Folio.tryCall('ui_state_get', { key: 'view' }, null);
            rememberedView = stored && stored.value;

            const storedTheme = await global.Folio.tryCall('ui_state_get', { key: 'theme' }, null);
            if (storedTheme && storedTheme.value && storedTheme.value !== state.theme) {
                setTheme(storedTheme.value, { persist: false });
            }
            await refreshRoots();
            await updateStatusBar();
        } catch (e) {
            console.error('folio: boot failed', e);
            global.UI.toast('Folio could not reach its core: ' + (e.message || e), {
                type: 'error', timeout: 0,
            });
            return;
        }

        // The corpus is what the app is for: with nothing registered, say so
        // where the document would be.
        if (!state.roots.length) {
            const panes = $('doc-panes');
            panes.classList.add('preview-hidden', 'empty');
            const empty = el('div', 'empty-state');
            empty.innerHTML =
                '<h2>Point Folio at your markdown</h2>' +
                '<p>Register the directories your agents read and write — a skills tree, a specs folder, ' +
                'a <code>TODO.md</code>. Everything inside is versioned automatically from then on, ' +
                'and agent edits arrive as proposals you review.</p>';
            const button = el('button', 'btn-primary', 'Add a root');
            button.addEventListener('click', () => global.Prefs.openAddRoot());
            empty.appendChild(button);
            $('editor-pane').appendChild(empty);
            $('editor-host').style.display = 'none';
        } else {
            // Reopen whatever was last in front.
            const remembered = await global.Folio.tryCall('ui_state_get', { key: 'lastDoc' }, null);
            let last = remembered && remembered.value;
            if (!last) {
                try { last = localStorage.getItem('folio.lastDoc'); } catch (e) { /* ignore */ }
            }
            const target = state.docs.find((d) => d.path === last) || state.docs[0];
            if (target) await openDoc(target.path);

            // Folio is meant to be opened every morning; come back to whatever
            // was in front, not always to a document.
            if (rememberedView === 'review' || rememberedView === 'today') {
                await showView(rememberedView, { remember: false });
            }
        }

        // Keep the status bar honest without polling the core hard.
        setInterval(updateStatusBar, 15000);

        global.UI.status('Ready');
    });

    global.App = App;
})(window);
