// CodeMirror 6, bundled into one global for the app to use.
//
// Monaco is an IDE; Folio is not. This exposes exactly the pieces the editor
// needs — markdown mode with frontmatter highlighting, a theme wired to the
// CSS variables, selection reporting for anchored comments, and gutter
// markers for validation findings — and nothing else.

import { EditorState, StateEffect, StateField, RangeSetBuilder, Compartment } from '@codemirror/state';
import {
    EditorView, keymap, highlightActiveLine, highlightActiveLineGutter,
    lineNumbers, drawSelection, rectangularSelection, crosshairCursor,
    highlightSpecialChars, placeholder, Decoration, gutter, GutterMarker, WidgetType,
} from '@codemirror/view';
import { defaultKeymap, history, historyKeymap, indentWithTab } from '@codemirror/commands';
import { highlightSelectionMatches } from '@codemirror/search';
import { markdown, markdownLanguage } from '@codemirror/lang-markdown';
import {
    syntaxHighlighting, HighlightStyle, bracketMatching,
    foldGutter, foldKeymap, indentOnInput,
} from '@codemirror/language';
import { tags } from '@lezer/highlight';

// Colours resolve to the theme's CSS variables, so a theme switch needs no
// editor rebuild — the same trick the rest of the app uses.
const v = (name, fallback) => `var(${name}, ${fallback})`;

const folioHighlight = HighlightStyle.define([
    { tag: tags.heading1, color: v('--text-highlight', '#fff'), fontWeight: '700', fontSize: '1.15em' },
    { tag: tags.heading2, color: v('--text-highlight', '#fff'), fontWeight: '700' },
    { tag: [tags.heading3, tags.heading4, tags.heading5, tags.heading6], color: v('--text-highlight', '#fff'), fontWeight: '600' },
    { tag: tags.strong, fontWeight: '700', color: v('--text-highlight', '#fff') },
    { tag: tags.emphasis, fontStyle: 'italic' },
    { tag: tags.strikethrough, textDecoration: 'line-through' },
    { tag: tags.link, color: v('--accent-primary', '#007acc') },
    { tag: tags.url, color: v('--accent-primary', '#007acc'), textDecoration: 'underline' },
    { tag: tags.monospace, color: v('--success-color', '#4ec9b0') },
    { tag: tags.quote, color: v('--text-secondary', '#858585'), fontStyle: 'italic' },
    { tag: tags.list, color: v('--accent-primary', '#007acc') },
    { tag: tags.contentSeparator, color: v('--border-light', '#505050') },
    { tag: tags.processingInstruction, color: v('--text-muted', '#6e6e6e') },
    { tag: tags.keyword, color: v('--accent-primary', '#007acc') },
    { tag: tags.atom, color: v('--warning-color', '#cca700') },
    { tag: tags.string, color: v('--success-color', '#4ec9b0') },
    { tag: tags.comment, color: v('--text-muted', '#6e6e6e'), fontStyle: 'italic' },
]);

const folioTheme = EditorView.theme({
    '&': {
        color: v('--text-primary', '#ccc'),
        backgroundColor: v('--bg-primary', '#1e1e1e'),
        height: '100%',
        fontSize: '13px',
    },
    '.cm-content': {
        fontFamily: v('--font-mono', 'Consolas, monospace'),
        padding: '10px 0',
        caretColor: v('--accent-primary', '#007acc'),
        lineHeight: '1.6',
    },
    '.cm-scroller': { overflow: 'auto', fontFamily: 'inherit' },
    '.cm-gutters': {
        backgroundColor: v('--bg-secondary', '#252526'),
        color: v('--text-muted', '#6e6e6e'),
        border: 'none',
        borderRight: `1px solid ${v('--border-color', '#3e3e42')}`,
    },
    '.cm-activeLineGutter': { backgroundColor: v('--bg-tertiary', '#2d2d30') },
    '.cm-activeLine': { backgroundColor: 'transparent' },
    '&.cm-focused .cm-activeLine': { backgroundColor: 'rgba(128,128,128,0.06)' },
    '.cm-selectionBackground, &.cm-focused .cm-selectionBackground, ::selection': {
        backgroundColor: v('--accent-secondary', '#264f78'),
    },
    '.cm-cursor, .cm-dropCursor': { borderLeftColor: v('--accent-primary', '#007acc') },
    '.cm-selectionMatch': { backgroundColor: 'rgba(0,122,204,0.20)' },
    '.cm-searchMatch': { backgroundColor: 'rgba(255,255,0,0.30)' },
    '.cm-searchMatch.cm-searchMatch-selected': { backgroundColor: 'rgba(255,140,0,0.55)' },
    '.cm-foldPlaceholder': {
        backgroundColor: v('--bg-tertiary', '#2d2d30'),
        border: `1px solid ${v('--border-color', '#3e3e42')}`,
        color: v('--text-secondary', '#858585'),
    },
    // The frontmatter block reads as metadata, not prose.
    '.cm-folio-frontmatter': {
        backgroundColor: 'rgba(128,128,128,0.08)',
        borderLeft: `2px solid ${v('--accent-primary', '#007acc')}`,
    },
    // An anchored comment underlines its text in the warning colour.
    '.cm-folio-anchor': {
        borderBottom: `2px solid ${v('--warning-color', '#cca700')}`,
        cursor: 'pointer',
    },
    '.cm-folio-anchor-outdated': {
        borderBottom: `2px dashed ${v('--text-muted', '#6e6e6e')}`,
        cursor: 'pointer',
    },
    '.cm-folio-ghost-caret': {
        display: 'inline-block',
        height: '1.25em',
        margin: '0 -1px -0.2em 0',
        borderLeft: `1px solid ${v('--accent-primary', '#007acc')}`,
        pointerEvents: 'none',
    },
});

// ---------------------------------------------------------------------------
// Frontmatter shading
// ---------------------------------------------------------------------------

const frontmatterLine = Decoration.line({ class: 'cm-folio-frontmatter' });

const frontmatterField = StateField.define({
    create: (state) => buildFrontmatter(state),
    update: (value, tr) => (tr.docChanged ? buildFrontmatter(tr.state) : value),
    provide: (f) => EditorView.decorations.from(f),
});

function buildFrontmatter(state) {
    const builder = new RangeSetBuilder();
    const first = state.doc.line(1);
    if (first.text.trim() !== '---') return builder.finish();
    let end = 0;
    for (let n = 2; n <= state.doc.lines; n++) {
        const text = state.doc.line(n).text.trim();
        if (text === '---' || text === '...') { end = n; break; }
    }
    if (!end) return builder.finish();
    for (let n = 1; n <= end; n++) builder.add(state.doc.line(n).from, state.doc.line(n).from, frontmatterLine);
    return builder.finish();
}

// ---------------------------------------------------------------------------
// Comment anchors
// ---------------------------------------------------------------------------

const setAnchors = StateEffect.define();

const anchorField = StateField.define({
    create: () => Decoration.none,
    update(value, tr) {
        value = value.map(tr.changes);
        for (const effect of tr.effects) {
            if (effect.is(setAnchors)) {
                const builder = new RangeSetBuilder();
                const sorted = [...effect.value].sort((a, b) => a.from - b.from);
                const max = tr.state.doc.length;
                let last = -1;
                for (const a of sorted) {
                    const from = Math.max(0, Math.min(a.from, max));
                    const to = Math.max(from, Math.min(a.to, max));
                    if (from === to || from < last) continue;
                    last = to;
                    builder.add(from, to, Decoration.mark({
                        class: a.outdated ? 'cm-folio-anchor-outdated' : 'cm-folio-anchor',
                        attributes: { 'data-comment-id': a.id, title: a.title || 'Open comment thread' },
                    }));
                }
                value = builder.finish();
            }
        }
        return value;
    },
    provide: (f) => EditorView.decorations.from(f),
});

// The preview-originated caret is positional context only. It never changes
// the editor selection or takes focus from the preview.
const setGhostCaret = StateEffect.define();

class GhostCaret extends WidgetType {
    toDOM() {
        const caret = document.createElement('span');
        caret.className = 'cm-folio-ghost-caret';
        caret.setAttribute('aria-hidden', 'true');
        return caret;
    }
}

const ghostCaretField = StateField.define({
    create: () => Decoration.none,
    update(value, tr) {
        value = value.map(tr.changes);
        for (const effect of tr.effects) {
            if (effect.is(setGhostCaret)) {
                if (effect.value === null) return Decoration.none;
                const at = Math.max(0, Math.min(effect.value, tr.state.doc.length));
                return Decoration.set([
                    Decoration.widget({ widget: new GhostCaret(), side: 1 }).range(at),
                ]);
            }
        }
        return value;
    },
    provide: (f) => EditorView.decorations.from(f),
});

// ---------------------------------------------------------------------------
// Validation gutter
// ---------------------------------------------------------------------------

const setFindings = StateEffect.define();

class FindingMarker extends GutterMarker {
    constructor(severity, message) {
        super();
        this.severity = severity;
        this.message = message;
    }
    toDOM() {
        const el = document.createElement('span');
        el.className = `cm-folio-finding cm-folio-finding--${this.severity}`;
        el.textContent = this.severity === 'error' ? '●' : this.severity === 'warning' ? '▲' : '○';
        el.title = this.message;
        return el;
    }
}

const findingsField = StateField.define({
    create: () => ({ byLine: new Map() }),
    update(value, tr) {
        for (const effect of tr.effects) {
            if (effect.is(setFindings)) {
                const byLine = new Map();
                for (const f of effect.value) {
                    if (!f.line) continue;
                    const existing = byLine.get(f.line);
                    // Worst severity wins the gutter slot.
                    if (!existing || rank(f.severity) < rank(existing.severity)) {
                        byLine.set(f.line, f);
                    }
                }
                return { byLine };
            }
        }
        return value;
    },
});

const rank = (s) => (s === 'error' ? 0 : s === 'warning' ? 1 : 2);

const findingsGutter = gutter({
    class: 'cm-folio-findings-gutter',
    lineMarker(view, line) {
        const state = view.state.field(findingsField, false);
        if (!state) return null;
        const lineNo = view.state.doc.lineAt(line.from).number;
        const finding = state.byLine.get(lineNo);
        return finding ? new FindingMarker(finding.severity, `${finding.rule}: ${finding.message}`) : null;
    },
    initialSpacer: () => new FindingMarker('info', ''),
});

// ---------------------------------------------------------------------------
// Public surface
// ---------------------------------------------------------------------------

const editable = new Compartment();

export function create(parent, options = {}) {
    const listeners = options.on || {};

    const view = new EditorView({
        parent,
        state: EditorState.create({
            doc: options.doc || '',
            extensions: [
                lineNumbers(),
                highlightActiveLineGutter(),
                highlightSpecialChars(),
                history(),
                foldGutter(),
                drawSelection(),
                indentOnInput(),
                bracketMatching(),
                highlightActiveLine(),
                highlightSelectionMatches(),
                rectangularSelection(),
                crosshairCursor(),
                keymap.of([...defaultKeymap, ...historyKeymap, ...foldKeymap, indentWithTab]),
                // Deliberately no `codeLanguages`: pulling every language grammar into
                // the bundle costs a megabyte to syntax-highlight fenced code in an
                // editor that sits beside a live preview which already highlights it.
                markdown({ base: markdownLanguage }),
                syntaxHighlighting(folioHighlight),
                folioTheme,
                EditorView.lineWrapping,
                frontmatterField,
                anchorField,
                ghostCaretField,
                findingsField,
                findingsGutter,
                placeholder(options.placeholder || ''),
                editable.of(EditorView.editable.of(options.editable !== false)),
                EditorView.updateListener.of((update) => {
                    if (update.docChanged && listeners.change) {
                        listeners.change(update.state.doc.toString());
                    }
                    if ((update.selectionSet || update.docChanged) && listeners.selection) {
                        const range = update.state.selection.main;
                        const line = update.state.doc.lineAt(range.head);
                        listeners.selection({
                            from: range.from,
                            to: range.to,
                            empty: range.empty,
                            text: range.empty ? '' : update.state.sliceDoc(range.from, range.to),
                            line: line.number,
                            column: range.head - line.from + 1,
                            coords: range.empty ? null : update.view.coordsAtPos(range.head),
                        });
                    }
                }),
                EditorView.domEventHandlers({
                    focus() {
                        view.dispatch({ effects: setGhostCaret.of(null) });
                    },
                    mousedown(event) {
                        const anchor = event.target.closest?.('[data-comment-id]');
                        if (anchor && listeners.anchorClick) {
                            listeners.anchorClick(anchor.getAttribute('data-comment-id'));
                        }
                    },
                }),
            ],
        }),
    });

    return {
        view,
        getValue: () => view.state.doc.toString(),
        setValue(text, { preserveCursor = false } = {}) {
            const selection = preserveCursor
                ? { anchor: Math.min(view.state.selection.main.anchor, text.length) }
                : undefined;
            view.dispatch({
                changes: { from: 0, to: view.state.doc.length, insert: text },
                selection,
                scrollIntoView: false,
            });
        },
        setEditable(on) {
            view.dispatch({ effects: editable.reconfigure(EditorView.editable.of(!!on)) });
        },
        setAnchors(anchors) {
            view.dispatch({ effects: setAnchors.of(anchors || []) });
        },
        setGhostCaret(offset) {
            const at = offset === null
                ? null
                : Math.max(0, Math.min(offset, view.state.doc.length));
            const effects = [setGhostCaret.of(at)];
            if (at !== null) effects.push(EditorView.scrollIntoView(at, { y: 'center' }));
            view.dispatch({ effects });
        },
        setFindings(findings) {
            view.dispatch({ effects: setFindings.of(findings || []) });
        },
        selection() {
            const range = view.state.selection.main;
            return {
                from: range.from,
                to: range.to,
                empty: range.empty,
                text: range.empty ? '' : view.state.sliceDoc(range.from, range.to),
            };
        },
        toByteOffset(offset) {
            const at = Math.max(0, Math.min(offset, view.state.doc.length));
            return new TextEncoder().encode(view.state.sliceDoc(0, at)).length;
        },
        fromByteOffset(offset) {
            const bytes = new TextEncoder().encode(view.state.doc.toString());
            const at = Math.max(0, Math.min(offset, bytes.length));
            return new TextDecoder().decode(bytes.slice(0, at)).length;
        },
        scrollTo(offset, { focus = true, selectionLength = 0 } = {}) {
            const at = Math.max(0, Math.min(offset, view.state.doc.length));
            const head = Math.min(at + selectionLength, view.state.doc.length);
            view.dispatch({ selection: { anchor: at, head }, scrollIntoView: true });
            if (focus) view.focus();
        },
        lineOffset(line) {
            const n = Math.max(1, Math.min(line, view.state.doc.lines));
            return view.state.doc.line(n).from;
        },
        focus: () => view.focus(),
        destroy: () => view.destroy(),
    };
}

export { EditorView, EditorState };
