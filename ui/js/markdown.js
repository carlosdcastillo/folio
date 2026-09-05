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

    if (hasMarked) {
        marked.setOptions({
            gfm: true,
            breaks: false,
            headerIds: false,
            mangle: false,
            highlight(code, language) {
                if (!hasHljs) return code;
                try {
                    // The guard. `hljs.highlight` throws on an unregistered
                    // language, and one bad fence would blank the preview.
                    if (language && hljs.getLanguage(language)) {
                        return hljs.highlight(code, { language, ignoreIllegals: true }).value;
                    }
                    return hljs.highlightAuto(code).value;
                } catch (e) {
                    console.warn('folio: highlight failed for language', language, e);
                    return code;
                }
            },
        });
    }

    function stripFrontmatter(text) {
        const source = String(text || '');
        if (!source.startsWith('---')) return { body: source, frontmatter: null };
        const lines = source.split('\n');
        if (lines[0].trim() !== '---') return { body: source, frontmatter: null };
        for (let i = 1; i < lines.length; i++) {
            const trimmed = lines[i].trim();
            if (trimmed === '---' || trimmed === '...') {
                return {
                    frontmatter: lines.slice(1, i).join('\n'),
                    body: lines.slice(i + 1).join('\n'),
                };
            }
        }
        return { body: source, frontmatter: null };
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
                return;
            }

            const { body, frontmatter } = stripFrontmatter(source);
            if (frontmatter && showFrontmatter) target.appendChild(frontmatterCard(frontmatter));

            const holder = document.createElement('div');
            try {
                holder.innerHTML = sanitize(hasMarked ? marked.parse(body) : escapeToPre(body));
            } catch (e) {
                console.error('folio: markdown render failed', e);
                holder.innerHTML = escapeToPre(body);
            }
            target.appendChild(holder);

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
        },

        stripFrontmatter,
    };

    function escapeToPre(text) {
        return '<pre>' + global.UI.escapeHtml(text) + '</pre>';
    }

    global.Markdown = Markdown;
})(window);
