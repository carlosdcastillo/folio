/* The corpus sidebar: roots as collapsible sections, documents with type
 * icons, and per-file badges for pending proposals and open comments. The
 * review inbox is pinned above the tree, because the morning ritual starts
 * there.
 */
(function (global) {
    'use strict';

    const { $, el, escapeHtml } = global.UI;

    let handlers = {};
    const collapsed = loadCollapsed();

    function loadCollapsed() {
        try {
            return new Set(JSON.parse(localStorage.getItem('folio.collapsedRoots') || '[]'));
        } catch (e) {
            return new Set();
        }
    }

    function saveCollapsed() {
        try {
            localStorage.setItem('folio.collapsedRoots', JSON.stringify(Array.from(collapsed)));
        } catch (e) { /* private mode */ }
    }

    function matchesFilter(doc, filter) {
        if (!filter) return true;
        const needle = filter.toLowerCase();
        return doc.relative.toLowerCase().includes(needle) ||
            doc.display.toLowerCase().includes(needle) ||
            doc.type.includes(needle);
    }

    const Sidebar = {
        init(callbacks) {
            handlers = callbacks || {};

            $('corpus-filter-input').addEventListener('input', (event) => {
                handlers.onFilter(event.target.value.trim());
            });

            $('sidebar-add-root').addEventListener('click', () => handlers.onAddRoot());
            $('review-inbox').addEventListener('click', () => handlers.onReview());

            const body = $('app-body');
            const hide = $('sidebar-hide-toggle');
            const show = $('sidebar-show-toggle');
            const setCollapsed = (on) => {
                body.classList.toggle('sidebar-collapsed', on);
                show.hidden = !on;
                hide.setAttribute('aria-expanded', String(!on));
                show.setAttribute('aria-expanded', String(!on));
                try { localStorage.setItem('folio.sidebarCollapsed', on ? '1' : '0'); } catch (e) { /* ignore */ }
            };
            hide.addEventListener('click', () => setCollapsed(true));
            show.addEventListener('click', () => setCollapsed(false));
            try {
                if (localStorage.getItem('folio.sidebarCollapsed') === '1') setCollapsed(true);
            } catch (e) { /* ignore */ }
        },

        render(state) {
            const tree = $('corpus-tree');
            tree.innerHTML = '';

            const pending = state.proposals
                ? state.proposals.filter((p) => p.status === 'pending' || p.status === 'conflict').length
                : 0;
            const inbox = $('review-inbox');
            inbox.hidden = pending === 0;
            $('review-inbox-count').textContent = String(pending);

            if (!state.roots.length) {
                const empty = el('div', 'sidebar-empty');
                empty.innerHTML =
                    '<p><strong>No roots yet.</strong></p>' +
                    '<p>Point Folio at the directories your agents read and write: ' +
                    '<code>~/.claude/skills</code>, a specs folder, <code>TODO.md</code>.</p>';
                tree.appendChild(empty);
                return;
            }

            for (const root of state.roots) {
                const docs = state.docs
                    .filter((d) => d.root_id === root.id && matchesFilter(d, state.filter))
                    .sort((a, b) => a.relative.localeCompare(b.relative));

                if (state.filter && !docs.length) continue;

                const section = el('div', 'root-section');
                if (collapsed.has(root.id) && !state.filter) section.classList.add('collapsed');

                const header = el('button', 'root-header');
                header.type = 'button';
                header.innerHTML =
                    '<span class="root-caret">▾</span>' +
                    '<span class="root-name">' + escapeHtml(root.label || root.display) + '</span>' +
                    '<span class="root-policy root-policy--' + escapeHtml(root.policy) + '">' +
                    escapeHtml(root.policy) + '</span>';
                header.title = root.display;
                header.addEventListener('click', () => {
                    if (collapsed.has(root.id)) collapsed.delete(root.id);
                    else collapsed.add(root.id);
                    saveCollapsed();
                    section.classList.toggle('collapsed');
                });
                section.appendChild(header);

                const list = el('div', 'root-docs');
                if (!docs.length) {
                    const none = el('div', 'sidebar-empty', 'Nothing indexed here yet.');
                    none.style.padding = '10px 12px';
                    list.appendChild(none);
                }

                for (const doc of docs) {
                    const row = el('button', 'doc-row');
                    row.type = 'button';
                    row.title = doc.display + '  ·  ' + doc.versions + ' version' +
                        (doc.versions === 1 ? '' : 's');
                    if (state.currentPath && state.currentPath === doc.path) row.classList.add('active');
                    if (!doc.exists) {
                        row.classList.add('missing');
                        row.title += '  ·  no longer on disk (history retained)';
                    }

                    const icon = el('span', 'doc-icon doc-icon--' + doc.type, global.UI.typeIcon(doc.type));

                    // Truncate the directory, never the file name: two files
                    // called `.../commands/...` are not the same document.
                    const relative = doc.relative || doc.display;
                    const cut = relative.lastIndexOf('/');
                    const name = el('span', 'doc-name');
                    if (cut >= 0) {
                        name.appendChild(el('span', 'doc-dir', relative.slice(0, cut + 1)));
                    }
                    name.appendChild(el('span', 'doc-base', relative.slice(cut + 1)));
                    const badges = el('span', 'doc-badges');

                    if (doc.pending_proposals > 0) {
                        badges.appendChild(el('span', 'badge', String(doc.pending_proposals)));
                    }
                    if (doc.open_comments > 0) {
                        badges.appendChild(el('span', 'badge badge--comment', String(doc.open_comments)));
                    }

                    row.appendChild(icon);
                    row.appendChild(name);
                    row.appendChild(badges);
                    row.addEventListener('click', () => handlers.onOpen(doc.path));
                    list.appendChild(row);
                }

                section.appendChild(list);
                tree.appendChild(section);
            }
        },
    };

    global.Sidebar = Sidebar;
})(window);
