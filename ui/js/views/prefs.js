/* Preferences: theme, fonts, roots and their write policies, MCP client
 * status, licence, store location. Plus the dialogs Tools opens.
 */
(function (global) {
    'use strict';

    const { $, el, escapeHtml } = global.UI;

    let app = null;

    const POLICY_LABELS = {
        auto: 'Auto — propose docs, skills, prompts; apply task lists directly',
        propose: 'Propose — every agent write waits for review',
        direct: 'Direct — agent writes apply immediately (always versioned)',
    };

    // -----------------------------------------------------------------------
    // Preferences
    // -----------------------------------------------------------------------

    async function open() {
        $('pref-theme').value = app.theme();
        $('pref-font-size').value = readFontSize();
        $('pref-preview').checked = global.DocView.previewOn();

        renderRoots();
        await renderStore();
        global.UI.openDialog('prefs-dialog');
    }

    function readFontSize() {
        try {
            const saved = parseInt(localStorage.getItem('folio.fontSize'), 10);
            if (saved > 0) return saved;
        } catch (e) { /* ignore */ }
        return 16;
    }

    function applyFontSize(px) {
        document.documentElement.style.setProperty('--content-font-size', px + 'px');
        try { localStorage.setItem('folio.fontSize', String(px)); } catch (e) { /* ignore */ }
    }

    function renderRoots() {
        const host = $('pref-roots');
        host.innerHTML = '';
        const roots = app.state().roots;

        if (!roots.length) {
            host.appendChild(el('div', 'today-empty', 'No roots registered yet.'));
            return;
        }

        for (const root of roots) {
            const row = el('div', 'root-manage-row');

            const label = el('div');
            label.appendChild(el('div', null, root.label));
            const path = el('div', 'root-manage-path', root.display);
            path.title = root.path + '  ·  ' + root.docs + ' documents';
            label.appendChild(path);
            row.appendChild(label);

            const select = document.createElement('select');
            for (const [value, text] of Object.entries(POLICY_LABELS)) {
                const option = document.createElement('option');
                option.value = value;
                option.textContent = value;
                option.title = text;
                select.appendChild(option);
            }
            select.value = root.policy;
            select.title = POLICY_LABELS[root.policy];
            select.addEventListener('change', async () => {
                try {
                    await global.Folio.call('set_root_policy', { id: root.id, policy: select.value });
                    select.title = POLICY_LABELS[select.value];
                    await app.refreshDocs();
                    global.UI.toast('Write policy for ' + root.label + ' is now ' + select.value + '.', {
                        type: 'success',
                    });
                } catch (e) {
                    global.UI.error(e, 'Could not change the policy');
                }
            });
            row.appendChild(select);

            const remove = el('button', 'find-btn', 'Remove');
            remove.type = 'button';
            remove.addEventListener('click', async () => {
                const go = await global.UI.confirm(
                    'Stop tracking ' + root.display + '?\n\n' +
                    'The files are untouched, and their history stays in the store — re-adding the root ' +
                    'reattaches the timeline instead of starting over.',
                    { title: 'Remove root', okLabel: 'Remove', danger: true }
                );
                if (!go) return;
                try {
                    await global.Folio.call('remove_root', { id: root.id });
                    await app.refreshRoots();
                    renderRoots();
                } catch (e) {
                    global.UI.error(e, 'Could not remove the root');
                }
            });
            row.appendChild(remove);

            host.appendChild(row);
        }
    }

    async function renderStore() {
        const status = await global.Folio.tryCall('status', {}, {});
        const boot = app.state().boot;
        const host = $('pref-store');
        host.innerHTML =
            '<div><strong>Location</strong> ' + escapeHtml(status.store_dir || boot.store_dir || '') + '</div>' +
            '<div><strong>Size</strong> ' + escapeHtml(global.UI.bytes(status.store_bytes || 0)) + '</div>' +
            '<div><strong>Roots</strong> ' + (status.roots || 0) +
            ' · <strong>Watching</strong> ' + (status.watching || 0) + '</div>' +
            '<div><strong>Pending proposals</strong> ' + (status.pending_proposals || 0) +
            ' · <strong>Open comments</strong> ' + (status.open_comments || 0) + '</div>';

        if (status.cloud_sync_warning) {
            const warning = el('div', 'proposal-conflict');
            warning.textContent =
                'The store is inside a cloud-synced folder (' + status.cloud_sync_warning + '). ' +
                'SQLite and file sync corrupt each other. Move it with --store, and back up by exporting instead.';
            host.appendChild(warning);
        }
    }


    // -----------------------------------------------------------------------
    // Add root
    // -----------------------------------------------------------------------

    function openAddRoot() {
        $('add-root-path').value = '';
        $('add-root-label').value = '';
        $('add-root-policy').value = 'auto';
        global.UI.openDialog('add-root-dialog');
    }

    async function confirmAddRoot() {
        const path = $('add-root-path').value.trim();
        if (!path) {
            global.UI.toast('Give Folio a directory or a file to track.', { type: 'warning' });
            return;
        }
        global.UI.loading(true, 'Indexing…');
        try {
            const result = await global.Folio.call('add_root', {
                path,
                label: $('add-root-label').value.trim(),
                policy: $('add-root-policy').value,
            });
            global.UI.closeDialog();
            await app.refreshRoots();
            global.UI.toast(
                'Added ' + result.root.label + ' · ' + result.indexed + ' file(s) indexed.',
                { type: 'success' }
            );
        } catch (e) {
            global.UI.error(e, 'Could not add the root');
        } finally {
            global.UI.loading(false);
        }
    }

    // -----------------------------------------------------------------------
    // New document
    // -----------------------------------------------------------------------

    const TEMPLATES = {
        doc: () => '# Title\n\n',
        skill: (name) =>
            '---\nname: ' + name + '\ndescription: What this skill does, in one sentence under 1024 characters.\n---\n\n' +
            'A short overview that routes the reader deeper, not a manual.\n\n' +
            '## Workflow\n\n1. First step.\n\n## Quality checks\n\n- [ ] It does what the description says.\n',
        prompt: () =>
            '---\nname: brief\nvariables: [topic, tone]\n---\n\n' +
            'Write about {{topic}} in a {{tone}} voice.\n',
        tasks: () => '# Tasks\n\n- [ ] First thing @you #inbox\n',
    };

    function openNewDoc() {
        const select = $('new-doc-root');
        select.innerHTML = '';
        for (const root of app.state().roots) {
            if (root.kind !== 'dir') continue;
            const option = document.createElement('option');
            option.value = root.path;
            option.textContent = root.label + '  (' + root.display + ')';
            select.appendChild(option);
        }
        if (!select.options.length) {
            global.UI.alert('Add a directory root first — Folio only writes inside registered roots.', 'New document');
            return;
        }
        $('new-doc-path').value = '';
        $('new-doc-template').value = 'doc';
        global.UI.openDialog('new-doc-dialog');
    }

    async function confirmNewDoc() {
        const root = $('new-doc-root').value;
        let relative = $('new-doc-path').value.trim().replace(/^[\/\\]+/, '');
        if (!relative) {
            global.UI.toast('Give the document a path.', { type: 'warning' });
            return;
        }
        if (!/\.[a-z0-9]+$/i.test(relative)) relative += '.md';

        const kind = $('new-doc-template').value;
        const stem = relative.split('/').pop().replace(/\.[^.]+$/, '');
        // A skill's frontmatter name must match its directory, so scaffold it
        // into one rather than producing a file that fails its own validator.
        if (kind === 'skill' && !/\/SKILL\.md$/i.test(relative)) {
            relative = stem + '/SKILL.md';
        }
        const name = kind === 'skill' ? relative.split('/').slice(-2)[0] : stem;

        try {
            const result = await global.Folio.call('create_doc', {
                path: root + '/' + relative,
                content: TEMPLATES[kind](name),
            });
            global.UI.closeDialog();
            await app.refreshDocs();
            await app.openDoc(result.path);
            global.UI.toast('Created ' + relative + '.', { type: 'success' });
        } catch (e) {
            global.UI.error(e, 'Could not create the document');
        }
    }

    // -----------------------------------------------------------------------
    // Corpus search
    // -----------------------------------------------------------------------

    function openSearch() {
        $('search-results').innerHTML = '';
        $('search-summary').textContent = '';
        global.UI.openDialog('search-dialog');
        setTimeout(() => $('search-query').focus(), 40);
    }

    async function runSearch() {
        const query = $('search-query').value.trim();
        if (!query) return;
        const host = $('search-results');
        host.innerHTML = '';
        try {
            const result = await global.Folio.call('search_docs', {
                query,
                regex: $('search-regex').checked,
                glob: $('search-glob').value.trim(),
                max_results: 300,
            });
            $('search-summary').textContent = result.count + ' match' + (result.count === 1 ? '' : 'es');
            if (!result.count) {
                host.appendChild(el('div', 'today-empty', 'Nothing matched.'));
                return;
            }
            for (const hit of result.matches) {
                const row = el('button', 'search-hit');
                row.type = 'button';
                row.appendChild(el('div', 'search-hit-path', hit.display + ':' + hit.line));
                const line = el('div', 'search-hit-line');
                line.innerHTML = highlight(hit.text, hit.start, hit.end);
                row.appendChild(line);
                row.addEventListener('click', () => {
                    global.UI.closeDialog();
                    app.openDoc(hit.path, { line: hit.line });
                });
                host.appendChild(row);
            }
        } catch (e) {
            global.UI.error(e, 'Search failed');
        }
    }

    function highlight(text, start, end) {
        if (start == null || end == null || end <= start || end > text.length) return escapeHtml(text);
        return escapeHtml(text.slice(0, start)) +
            '<mark>' + escapeHtml(text.slice(start, end)) + '</mark>' +
            escapeHtml(text.slice(end));
    }

    // -----------------------------------------------------------------------
    // Render prompt
    // -----------------------------------------------------------------------

    let renderPath = null;

    async function openRender() {
        const doc = global.DocView.current();
        if (!doc) {
            global.UI.alert('Open a prompt first.', 'Render prompt');
            return;
        }
        if (doc.type !== 'prompt') {
            const go = await global.UI.confirm(
                'This document is typed as a ' + global.UI.typeLabel(doc.type).toLowerCase() +
                ', not a prompt. Render it anyway?',
                { title: 'Render prompt', okLabel: 'Render anyway' }
            );
            if (!go) return;
        }
        renderPath = doc.path;

        const host = $('render-variables');
        host.innerHTML = '';
        $('render-output').textContent = '';

        let declared = [];
        try {
            const probe = await global.Folio.call('render_prompt', { path: renderPath, variables: {} });
            declared = probe.declared || [];
            $('render-output').textContent = probe.rendered;
        } catch (e) {
            // The expected path: a prompt with required slots refuses to render
            // with a hole in it, which is exactly what we want to ask about.
            const list = await global.Folio.tryCall('validate_doc', { path: renderPath }, null);
            declared = [];
            if (list) {
                $('render-output').textContent = e.message;
            }
        }

        const doc2 = await global.Folio.tryCall('read_doc', { path: renderPath }, null);
        const slots = new Set();
        if (doc2) {
            const body = doc2.content.slice(0);
            const pattern = /\{\{\s*([a-zA-Z0-9_.-]+)\s*\}\}/g;
            let match;
            while ((match = pattern.exec(body)) !== null) slots.add(match[1]);
        }
        for (const spec of declared) slots.add(spec.name);

        if (!slots.size) {
            host.appendChild(el('div', 'today-empty', 'This prompt declares no variables.'));
        }
        for (const name of slots) {
            const spec = declared.find((d) => d.name === name);
            const label = document.createElement('label');
            label.textContent = name + (spec && spec.description ? ' — ' + spec.description : '');
            const input = document.createElement('input');
            input.type = 'text';
            input.dataset.variable = name;
            input.value = spec && spec.default ? spec.default : '';
            input.placeholder = spec && spec.required === false ? '(optional)' : '';
            label.appendChild(input);
            host.appendChild(label);
        }

        global.UI.openDialog('render-dialog');
    }

    async function runRender() {
        if (!renderPath) return;
        const variables = {};
        for (const input of $('render-variables').querySelectorAll('input[data-variable]')) {
            if (input.value !== '') variables[input.dataset.variable] = input.value;
        }
        try {
            const result = await global.Folio.call('render_prompt', { path: renderPath, variables });
            $('render-output').textContent = result.rendered;
        } catch (e) {
            $('render-output').textContent = e.message;
        }
    }

    // -----------------------------------------------------------------------
    // MCP status
    // -----------------------------------------------------------------------

    async function openMcp() {
        const status = await global.Folio.tryCall('status', {}, {});
        const host = $('mcp-body');
        host.innerHTML = '';

        const intro = el('div', 'pref-section');
        intro.innerHTML =
            '<h4>Connected clients</h4>' +
            (status.clients && status.clients.length
                ? ''
                : '<div class="today-empty">No MCP client has connected yet.</div>');
        for (const client of status.clients || []) {
            const row = el('div', 'mcp-client-row');
            row.appendChild(el('span', 'status-connection status-connection--connected', '●'));
            row.appendChild(el('span', 'mcp-client-name', client.name));
            row.appendChild(el('span', 'mcp-client-mode', client.mode));
            row.appendChild(el('span', 'comment-time', global.UI.relativeTime(new Date(client.last_seen).toISOString())));
            intro.appendChild(row);
        }
        host.appendChild(intro);

        const config = el('div', 'pref-section');
        config.innerHTML =
            '<h4>Client configuration</h4>' +
            '<p class="dialog-hint">Point any MCP client at the same binary. With Folio open, tool calls are ' +
            'bridged to this window so proposals appear live; with it closed, the bridge runs headless and ' +
            'proposals queue in the store for the next time you open it.</p>';
        const pre = el('pre', 'render-output', mcpConfig());
        config.appendChild(pre);
        host.appendChild(config);

        global.UI.openDialog('mcp-dialog');
    }

    function mcpConfig() {
        return JSON.stringify({
            mcpServers: { folio: { command: 'folio', args: ['mcp'] } },
        }, null, 2);
    }

    // -----------------------------------------------------------------------
    // Validate all
    // -----------------------------------------------------------------------

    async function validateAll() {
        global.UI.loading(true, 'Validating skills…');
        let result;
        try {
            result = await global.Folio.call('validate_all', { type: 'skill' });
        } catch (e) {
            global.UI.loading(false);
            global.UI.error(e, 'Validation failed');
            return;
        }
        global.UI.loading(false);

        const body = $('validate-body');
        $('validate-dialog').querySelector('h3').textContent = 'Validation Report';
        body.innerHTML = '';

        if (!result.reports.length) {
            body.appendChild(el('div', 'today-empty',
                'No skills in the corpus. A skill is a SKILL.md with `name` and `description` in its frontmatter.'));
        }

        for (const report of result.reports) {
            const card = el('div', 'proposal-head');
            const title = el('div', 'proposal-head-title');
            title.appendChild(el('span', 'proposal-path', report.display));
            const chip = report.errors ? 'rejected' : report.warnings ? 'pending' : 'accepted';
            title.appendChild(el('span', 'status-chip status-chip--' + chip,
                report.errors ? report.errors + ' errors' : report.warnings ? report.warnings + ' warnings' : 'clean'));
            card.appendChild(title);

            for (const finding of report.findings) {
                if (finding.severity === 'info' && !report.errors && !report.warnings) continue;
                const row = el('button', 'finding-row');
                row.type = 'button';
                row.appendChild(el('span', 'finding-icon finding-icon--' + finding.severity,
                    finding.severity === 'error' ? '●' : finding.severity === 'warning' ? '▲' : '○'));
                const detail = el('div');
                detail.appendChild(el('span', 'finding-message', finding.message));
                const meta = [finding.rule];
                if (finding.file) meta.push(finding.file);
                if (finding.line) meta.push('line ' + finding.line);
                detail.appendChild(el('span', 'finding-meta', meta.join('  ·  ')));
                row.appendChild(detail);
                row.addEventListener('click', () => {
                    global.UI.closeDialog();
                    app.openDoc(report.path, { line: finding.line });
                });
                card.appendChild(row);
            }
            body.appendChild(card);
        }

        $('validate-summary').textContent =
            result.reports.length + ' skill(s) · ' + result.errors + ' error(s) · ' + result.warnings + ' warning(s)';
        global.UI.openDialog('validate-dialog');
    }

    // -----------------------------------------------------------------------

    const Prefs = {
        init(application) {
            app = application;
            applyFontSize(readFontSize());

            $('add-root-confirm').addEventListener('click', confirmAddRoot);
            $('add-root-path').addEventListener('keydown', (e) => {
                if (e.key === 'Enter') confirmAddRoot();
            });
            $('new-doc-confirm').addEventListener('click', confirmNewDoc);
            $('new-doc-path').addEventListener('keydown', (e) => {
                if (e.key === 'Enter') confirmNewDoc();
            });

            $('search-run').addEventListener('click', runSearch);
            $('search-query').addEventListener('keydown', (e) => {
                if (e.key === 'Enter') runSearch();
            });

            $('render-run').addEventListener('click', runRender);
            $('render-copy').addEventListener('click', async () => {
                const ok = await global.Folio.copyToClipboard($('render-output').textContent);
                global.UI.toast(ok ? 'Rendered prompt copied.' : 'Could not reach the clipboard.', {
                    type: ok ? 'success' : 'error',
                });
            });

            $('export-copy').addEventListener('click', async () => {
                const ok = await global.Folio.copyToClipboard($('export-output').textContent);
                global.UI.toast(ok ? 'Patch series copied.' : 'Could not reach the clipboard.', {
                    type: ok ? 'success' : 'error',
                });
            });

            $('mcp-copy-config').addEventListener('click', async () => {
                const ok = await global.Folio.copyToClipboard(mcpConfig());
                global.UI.toast(ok ? 'Client config copied.' : 'Could not reach the clipboard.', {
                    type: ok ? 'success' : 'error',
                });
            });

            $('pref-add-root').addEventListener('click', openAddRoot);
            $('pref-theme').addEventListener('change', (e) => app.setTheme(e.target.value));
            $('pref-font-size').addEventListener('change', (e) => {
                const px = Math.max(12, Math.min(24, parseInt(e.target.value, 10) || 16));
                e.target.value = px;
                applyFontSize(px);
            });
            $('pref-preview').addEventListener('change', (e) => global.DocView.setPreview(e.target.checked));
        },

        open,
        openAddRoot,
        openNewDoc,
        openSearch,
        openRender,
        openMcp,
        validateAll,
    };

    global.Prefs = Prefs;
})(window);
