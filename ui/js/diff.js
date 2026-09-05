/* Rendering for the core's prose-aware diff.
 *
 * The engine hands over paragraph-grouped hunks whose rows carry word-level
 * token ranges. This draws them, and — in review mode — puts an accept/reject
 * control on each hunk, which is what makes hunk-level review possible.
 */
(function (global) {
    'use strict';

    const { el, escapeHtml } = global.UI;

    /** A row's text with its changed word ranges wrapped for emphasis. */
    function rowText(row) {
        if (!row.spans || !row.spans.length) {
            return escapeHtml(row.text);
        }
        // Spans are byte offsets from Rust. For the ASCII-dominant markdown
        // Folio holds these line up with JS string indices; when they do not,
        // fall back rather than slicing a character in half.
        const bytes = new TextEncoder().encode(row.text);
        let html = '';
        for (const span of row.spans) {
            if (span.start > bytes.length || span.end > bytes.length || span.end < span.start) {
                return escapeHtml(row.text);
            }
            const piece = new TextDecoder().decode(bytes.slice(span.start, span.end));
            html += span.changed
                ? '<span class="diff-word">' + escapeHtml(piece) + '</span>'
                : escapeHtml(piece);
        }
        return html;
    }

    function renderRow(row) {
        const line = el('div', 'diff-line diff-line--' +
            (row.kind === 'insert' ? 'add' : row.kind === 'delete' ? 'del' : 'context'));

        const number = el('span', 'diff-line-no');
        const marker = row.kind === 'insert' ? '+' : row.kind === 'delete' ? '-' : ' ';
        const shown = row.kind === 'insert' ? row.new_line : row.old_line;
        number.textContent = (shown == null ? '' : String(shown + 1)) + ' ' + marker;

        const content = el('span', 'diff-line-text');
        content.innerHTML = rowText(row);

        line.appendChild(number);
        line.appendChild(content);
        return line;
    }

    const Diff = {
        /**
         * Draw a diff.
         *
         * `options.review` turns on per-hunk accept/reject, and
         * `options.onSelectionChange(acceptedIndices)` reports the running
         * selection so the footer can say what accepting would do.
         */
        render(container, diff, options) {
            const { review = false, onSelectionChange = null } = options || {};
            container.innerHTML = '';
            container.className = 'diff';

            if (!diff || !diff.hunks || !diff.hunks.length) {
                container.appendChild(el('div', 'diff-empty', 'No differences.'));
                if (onSelectionChange) onSelectionChange([]);
                return { accepted: () => [] };
            }

            // Everything starts accepted: the common case is accepting the whole
            // proposal, and rejecting a hunk should be the deliberate act.
            const accepted = new Set(diff.hunks.map((h) => h.index));

            function announce() {
                if (onSelectionChange) onSelectionChange(Array.from(accepted).sort((a, b) => a - b));
            }

            for (const hunk of diff.hunks) {
                const node = el('div', 'diff-hunk');
                node.dataset.hunk = String(hunk.index);

                const header = el('div', 'diff-hunk-header');
                header.appendChild(el('span', 'diff-hunk-range',
                    '@@ -' + (hunk.old_start + 1) + ',' + hunk.old_lines +
                    ' +' + (hunk.new_start + 1) + ',' + hunk.new_lines + ' @@'));
                header.appendChild(el('span', 'diff-hunk-title', hunk.header || ''));

                const stat = el('span', 'diff-hunk-stat');
                stat.innerHTML =
                    '<span class="added">+' + hunk.added + '</span> ' +
                    '<span class="removed">-' + hunk.removed + '</span>';
                header.appendChild(stat);

                if (review) {
                    const actions = el('div', 'hunk-actions');
                    const acceptBtn = el('button', 'hunk-btn chosen-accept', 'Accept');
                    const rejectBtn = el('button', 'hunk-btn', 'Reject');
                    acceptBtn.type = 'button';
                    rejectBtn.type = 'button';

                    function sync() {
                        const on = accepted.has(hunk.index);
                        acceptBtn.classList.toggle('chosen-accept', on);
                        rejectBtn.classList.toggle('chosen-reject', !on);
                        node.classList.toggle('rejected', !on);
                    }
                    acceptBtn.addEventListener('click', () => {
                        accepted.add(hunk.index);
                        sync();
                        announce();
                    });
                    rejectBtn.addEventListener('click', () => {
                        accepted.delete(hunk.index);
                        sync();
                        announce();
                    });

                    actions.appendChild(acceptBtn);
                    actions.appendChild(rejectBtn);
                    header.appendChild(actions);
                    sync();
                }

                node.appendChild(header);
                for (const row of hunk.rows) node.appendChild(renderRow(row));
                container.appendChild(node);
            }

            announce();
            return {
                accepted: () => Array.from(accepted).sort((a, b) => a - b),
                total: diff.hunks.length,
            };
        },

        /** "+12 −3 across 2 hunks", for summaries. */
        summary(diff) {
            if (!diff) return '';
            const hunks = diff.hunks ? diff.hunks.length : 0;
            if (!hunks) return 'no changes';
            return '+' + diff.added + ' −' + diff.removed +
                ' across ' + hunks + (hunks === 1 ? ' hunk' : ' hunks');
        },
    };

    global.DiffView = Diff;
})(window);
