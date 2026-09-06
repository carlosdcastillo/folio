/* The preview render pipeline: marked -> DOMPurify -> highlight.js -> KaTeX.
 *
 * The one hard-won rule, inherited from Alpaca Assist: always check
 * `hljs.getLanguage()` before calling `hljs.highlight()`. An unknown language
 * tag in a fenced block must never be able to abort a render.
 */
(function (global) {
    'use strict';

    const hasMarked = typeof marked !== 'undefined';
    const hasPurify = typeof DOMPurify !== 'undefined';
    const hasHljs = typeof hljs !== 'undefined';
    const hasKatex = typeof renderMathInElement !== 'undefined';

    if (hasMarked) marked.setOptions({ gfm: true, breaks: false });

    function stripFrontmatter(text) {
        const source = String(text || '');
        const firstEnd = source.indexOf('\n');
        const first = source.slice(0, firstEnd < 0 ? source.length : firstEnd).replace(/\r$/, '');
        if (first.trim() !== '---') return { body: source, frontmatter: null, bodyOffset: 0 };

        let lineStart = firstEnd < 0 ? source.length : firstEnd + 1;
        while (lineStart < source.length) {
            const lineEnd = source.indexOf('\n', lineStart);
            const end = lineEnd < 0 ? source.length : lineEnd;
            const trimmed = source.slice(lineStart, end).replace(/\r$/, '').trim();
            if (trimmed === '---' || trimmed === '...') {
                const bodyOffset = lineEnd < 0 ? source.length : lineEnd + 1;
                return {
                    frontmatter: source.slice(firstEnd + 1, lineStart).replace(/\r?\n$/, ''),
                    body: source.slice(bodyOffset),
                    bodyOffset,
                };
            }
            if (lineEnd < 0) break;
            lineStart = lineEnd + 1;
        }
        return { body: source, frontmatter: null, bodyOffset: 0 };
    }

    /** Frontmatter is metadata; it gets a card, not a horizontal rule. */
    function frontmatterCard(yaml) {
        const card = document.createElement('div');
        card.className = 'frontmatter-card';
        for (const line of yaml.split('\n')) {
            if (!line.trim()) continue;
            const row = document.createElement('div');
            const colon = line.indexOf(':');
            if (colon > 0 && !line.startsWith(' ') && !line.startsWith('-')) {
                const key = document.createElement('span');
                key.className = 'fm-key';
                key.textContent = line.slice(0, colon + 1) + ' ';
                const value = document.createElement('span');
                value.className = 'fm-value';
                value.textContent = line.slice(colon + 1).trim();
                row.appendChild(key);
                row.appendChild(value);
            } else {
                const value = document.createElement('span');
                value.className = 'fm-value';
                value.textContent = line;
                row.appendChild(value);
            }
            card.appendChild(row);
        }
        return card;
    }

    function sanitize(html) {
        if (!hasPurify) return html;
        return DOMPurify.sanitize(html, {
            ADD_ATTR: ['target', 'rel', 'class', 'data-line', 'checked', 'disabled', 'type'],
        });
    }

    /** Render top-level tokens separately so their exact `raw` ranges survive. */
    function renderMapped(holder, body, bodyOffset) {
        if (!hasMarked) {
            holder.innerHTML = escapeToPre(body);
            const pre = holder.querySelector('pre');
            if (pre) setSourceRange(pre, bodyOffset, bodyOffset + body.length);
            return;
        }

        const tokens = marked.lexer(body);
        let cursor = 0;
        for (const token of tokens) {
            const raw = token.raw || '';
            // Marked's block lexer consumes the input in order. Verify that
            // invariant rather than searching for repeated text and guessing.
            if (body.slice(cursor, cursor + raw.length) !== raw) {
                throw new Error('marked token ranges did not consume the source in order');
            }
            const start = bodyOffset + cursor;
            cursor += raw.length;
            if (token.type === 'space' || token.type === 'def') continue;

            const fragment = document.createElement('template');
            const one = [token];
            one.links = tokens.links;
            fragment.innerHTML = sanitize(marked.parser(one));
            for (const child of Array.from(fragment.content.children)) {
                setSourceRange(child, start, start + raw.length);
            }
            holder.appendChild(fragment.content);
        }
        if (cursor !== body.length) {
            throw new Error('marked token ranges did not cover the source');
        }
    }

    function setSourceRange(element, start, end) {
        element.dataset.sourceStart = String(start);
        element.dataset.sourceEnd = String(end);
    }

    function sourceBlocks(target) {
        return Array.from(target.querySelectorAll('[data-source-start][data-source-end]'))
            .map((element) => ({
                element,
                start: Number(element.dataset.sourceStart),
                end: Number(element.dataset.sourceEnd),
            }))
            .sort((a, b) => a.start - b.start);
    }

    function containingBlock(target, node) {
        const element = node && (node.nodeType === Node.ELEMENT_NODE ? node : node.parentElement);
        const block = element && element.closest('[data-source-start][data-source-end]');
        return block && target.contains(block) ? block : null;
    }

    /** Map rendered UTF-16 character boundaries monotonically into raw source. */
    function characterMap(block, source) {
        const start = Number(block.dataset.sourceStart);
        const raw = source.slice(start, Number(block.dataset.sourceEnd));
        const rendered = textNodes(block).map((node) => node.data).join('');
        const starts = [];
        const ends = [];
        let cursor = 0;
        for (let i = 0; i < rendered.length; i++) {
            const at = raw.indexOf(rendered[i], cursor);
            if (at < 0) return null;
            starts.push(start + at);
            ends.push(start + at + 1);
            cursor = at + 1;
        }
        return { rendered, starts, ends };
    }

    function textOffset(block, node, offset) {
        let at = 0;
        for (const text of textNodes(block)) {
            if (text === node) return at + Math.max(0, Math.min(offset, text.data.length));
            at += text.data.length;
        }
        return null;
    }

    function mapBoundary(target, node, offset, side) {
        const block = containingBlock(target, node);
        if (!block) return null;
        const map = characterMap(block, target._folioSource || '');
        const renderedOffset = map && textOffset(block, node, offset);
        if (!map || renderedOffset === null || renderedOffset > map.rendered.length) return null;
        let sourceOffset;
        if (!map.rendered.length) sourceOffset = Number(block.dataset.sourceStart);
        else if (side === 'end') sourceOffset = renderedOffset ? map.ends[renderedOffset - 1] : map.starts[0];
        else sourceOffset = renderedOffset < map.rendered.length ? map.starts[renderedOffset] : map.ends.at(-1);
        return { offset: sourceOffset, block };
    }

    function mapRange(target, range) {
        if (!range || range.collapsed || !target.contains(range.commonAncestorContainer)) return null;
        const start = mapBoundary(target, range.startContainer, range.startOffset, 'start');
        const end = mapBoundary(target, range.endContainer, range.endOffset, 'end');
        if (!start || !end || end.offset <= start.offset) return null;
        return {
            from: start.offset,
            to: end.offset,
            text: (target._folioSource || '').slice(start.offset, end.offset),
        };
    }

    function mapPoint(target, x, y) {
        const caret = document.caretPositionFromPoint
            ? document.caretPositionFromPoint(x, y)
            : document.caretRangeFromPoint?.(x, y);
        if (!caret) return null;
        const node = caret.offsetNode || caret.startContainer;
        const offset = caret.offset ?? caret.startOffset;
        const mapped = mapBoundary(target, node, offset, 'start');
        return mapped ? mapped.offset : null;
    }

    /** Map a raw source offset to the nearest rendered text boundary. */
    function mapOffset(target, offset) {
        const blocks = sourceBlocks(target);
        let block = blockForOffset(target, offset);
        if (!block && blocks.length) {
            block = blocks.reduce((nearest, candidate) => {
                const distance = offset < candidate.start
                    ? candidate.start - offset
                    : Math.max(0, offset - candidate.end);
                return !nearest || distance < nearest.distance
                    ? { element: candidate.element, distance }
                    : nearest;
            }, null).element;
        }
        if (!block) return null;

        const nodes = textNodes(block);
        const map = characterMap(block, target._folioSource || '');
        if (!map || !map.rendered.length || !nodes.length) return { block, node: null, offset: 0 };

        let renderedOffset = 0;
        let bestDistance = Infinity;
        for (let i = 0; i < map.rendered.length; i++) {
            const beforeDistance = Math.abs(offset - map.starts[i]);
            if (beforeDistance < bestDistance) {
                renderedOffset = i;
                bestDistance = beforeDistance;
            }
            const afterDistance = Math.abs(offset - map.ends[i]);
            if (afterDistance < bestDistance) {
                renderedOffset = i + 1;
                bestDistance = afterDistance;
            }
        }

        for (const node of nodes) {
            if (renderedOffset <= node.data.length) return { block, node, offset: renderedOffset };
            renderedOffset -= node.data.length;
        }
        const node = nodes.at(-1);
        return { block, node, offset: node.data.length };
    }

    function blockForOffset(target, offset) {
        const blocks = sourceBlocks(target);
        let low = 0;
        let high = blocks.length - 1;
        while (low <= high) {
            const mid = (low + high) >> 1;
            const block = blocks[mid];
            if (offset < block.start) high = mid - 1;
            else if (offset >= block.end) low = mid + 1;
            else return block.element;
        }
        return null;
    }

    function textNodes(block) {
        const walker = document.createTreeWalker(block, NodeFilter.SHOW_TEXT);
        const nodes = [];
        let node;
        while ((node = walker.nextNode())) {
            // Marked inserts formatting newlines between structural tags. They
            // are not visible prose and have no corresponding source character.
            if (/^[\t\r\n ]*$/.test(node.data) && /[\r\n]/.test(node.data)) continue;
            nodes.push(node);
        }
        return nodes;
    }

    function clearSelection(target) {
        for (const selection of target.querySelectorAll('.preview-ghost-selection')) {
            const parent = selection.parentNode;
            selection.replaceWith(...selection.childNodes);
            parent.normalize();
        }
    }

    /** Decorate the rendered characters covered by an editor selection. */
    function applySelection(target, from, to) {
        clearSelection(target);
        if (to <= from) return;
        const source = target._folioSource || '';
        for (const block of sourceBlocks(target)) {
            if (to <= block.start || from >= block.end) continue;
            const map = characterMap(block.element, source);
            if (!map) continue;
            let renderedAt = 0;
            for (const node of textNodes(block.element)) {
                const pieces = [];
                let pieceStart = null;
                for (let i = 0; i < node.data.length; i++) {
                    const sourceAt = map.starts[renderedAt + i];
                    const covered = sourceAt >= from && sourceAt < to;
                    if (covered && pieceStart === null) pieceStart = i;
                    if (!covered && pieceStart !== null) {
                        pieces.push([pieceStart, i]);
                        pieceStart = null;
                    }
                }
                if (pieceStart !== null) pieces.push([pieceStart, node.data.length]);
                renderedAt += node.data.length;
                for (const [start, end] of pieces.reverse()) {
                    const span = document.createElement('span');
                    span.className = 'preview-ghost-selection';
                    const range = document.createRange();
                    range.setStart(node, start);
                    range.setEnd(node, end);
                    range.surroundContents(span);
                }
            }
        }
    }

    /** Decorate the rendered characters covered by non-overlapping anchors. */
    function applyAnchors(target, anchors) {
        const source = target._folioSource || '';
        const occupied = [];
        for (const anchor of [...(anchors || [])].sort((a, b) => a.from - b.from)) {
            if (occupied.some((range) => anchor.from < range.to && anchor.to > range.from)) continue;
            occupied.push(anchor);
            for (const block of sourceBlocks(target)) {
                if (anchor.to <= block.start || anchor.from >= block.end) continue;
                const map = characterMap(block.element, source);
                if (!map) continue;
                let renderedAt = 0;
                for (const node of textNodes(block.element)) {
                    const pieces = [];
                    let pieceStart = null;
                    for (let i = 0; i < node.data.length; i++) {
                        const sourceAt = map.starts[renderedAt + i];
                        const covered = sourceAt >= anchor.from && sourceAt < anchor.to;
                        if (covered && pieceStart === null) pieceStart = i;
                        if (!covered && pieceStart !== null) {
                            pieces.push([pieceStart, i]);
                            pieceStart = null;
                        }
                    }
                    if (pieceStart !== null) pieces.push([pieceStart, node.data.length]);
                    renderedAt += node.data.length;
                    for (const [from, to] of pieces.reverse()) {
                        const span = document.createElement('span');
                        span.className = anchor.outdated ? 'preview-anchor-outdated' : 'preview-anchor';
                        span.dataset.commentId = anchor.id;
                        span.title = anchor.title || 'Open comment thread';
                        const range = document.createRange();
                        range.setStart(node, from);
                        range.setEnd(node, to);
                        range.surroundContents(span);
                    }
                }
            }
        }
    }

    /** Wrap `<pre><code>` in the Alpaca Assist code block, with a copy button. */
    function decorateCodeBlocks(container) {
        for (const pre of Array.from(container.querySelectorAll('pre'))) {
            if (pre.closest('.code-block')) continue;
            const code = pre.querySelector('code');
            const language = code
                ? (Array.from(code.classList).find((c) => c.startsWith('language-')) || '').replace('language-', '')
                : '';

            const block = document.createElement('div');
            block.className = 'code-block';

            const header = document.createElement('div');
            header.className = 'code-header';
            const label = document.createElement('span');
            label.className = 'lang';
            label.textContent = language || 'text';
            const copy = document.createElement('button');
            copy.className = 'copy-btn';
            copy.type = 'button';
            copy.textContent = 'Copy';
            copy.addEventListener('click', async () => {
                const ok = await global.Folio.copyToClipboard(code ? code.textContent : pre.textContent);
                copy.textContent = ok ? 'Copied' : 'Failed';
                setTimeout(() => { copy.textContent = 'Copy'; }, 1400);
            });
            header.appendChild(label);
            header.appendChild(copy);

            pre.parentNode.insertBefore(block, pre);
            block.appendChild(header);
            block.appendChild(pre);
        }
    }

    function highlightCodeBlocks(container) {
        if (!hasHljs) return;
        for (const code of container.querySelectorAll('pre code')) {
            const language = (Array.from(code.classList)
                .find((name) => name.startsWith('language-')) || '').replace('language-', '');
            try {
                // The guard. `hljs.highlight` throws on an unregistered
                // language, and one bad fence must not blank the preview.
                const result = language && hljs.getLanguage(language)
                    ? hljs.highlight(code.textContent, { language, ignoreIllegals: true })
                    : hljs.highlightAuto(code.textContent);
                code.innerHTML = result.value;
                code.classList.add('hljs');
            } catch (e) {
                console.warn('folio: highlight failed for language', language, e);
            }
        }
    }

    function decorateTaskLists(container) {
        for (const item of container.querySelectorAll('li')) {
            const first = item.firstElementChild;
            if (first && first.tagName === 'INPUT' && first.type === 'checkbox') {
                item.classList.add('task-list-item');
                first.disabled = true;
            }
        }
    }

    const Markdown = {
        /**
         * Render markdown into `target`. Frontmatter is lifted out into its own
         * card so the preview shows a skill's metadata as metadata.
         */
        render(target, text, options) {
            const { showFrontmatter = true } = options || {};
            target.innerHTML = '';
            const source = String(text || '');
            if (!source.trim()) {
                const empty = document.createElement('p');
                empty.style.color = 'var(--text-secondary)';
                empty.textContent = 'This document is empty.';
                target.appendChild(empty);
                target._folioSource = source;
                return;
            }

            const { body, frontmatter, bodyOffset } = stripFrontmatter(source);
            if (frontmatter && showFrontmatter) target.appendChild(frontmatterCard(frontmatter));

            const holder = document.createElement('div');
            try {
                renderMapped(holder, body, bodyOffset);
            } catch (e) {
                console.error('folio: markdown render failed', e);
                holder.innerHTML = escapeToPre(body);
            }
            target.appendChild(holder);

            highlightCodeBlocks(target);
            decorateCodeBlocks(target);
            decorateTaskLists(target);

            if (hasKatex) {
                try {
                    renderMathInElement(target, {
                        delimiters: [
                            { left: '$$', right: '$$', display: true },
                            { left: '\\[', right: '\\]', display: true },
                            { left: '\\(', right: '\\)', display: false },
                            { left: '$', right: '$', display: false },
                        ],
                        throwOnError: false,
                    });
                } catch (e) {
                    console.warn('folio: KaTeX pass failed', e);
                }
            }

            // External links open in the user's browser, not in the app shell.
            for (const link of target.querySelectorAll('a[href^="http"]')) {
                link.setAttribute('target', '_blank');
                link.setAttribute('rel', 'noopener noreferrer');
            }
            target._folioSource = source;
        },

        stripFrontmatter,
        mapRange,
        mapPoint,
        mapOffset,
        blockForOffset,
        clearSelection,
        applySelection,
        applyAnchors,
    };

    function escapeToPre(text) {
        return '<pre>' + global.UI.escapeHtml(text) + '</pre>';
    }

    global.Markdown = Markdown;
})(window);
