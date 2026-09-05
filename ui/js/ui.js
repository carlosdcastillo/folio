/* Shell primitives: dialogs, toasts, the message dialog, menus, splitters.
 *
 * Two rules from Alpaca Assist are load-bearing here and are not negotiable:
 * native `alert` / `confirm` / `prompt` are banned, because they show the page
 * origin and centre on the screen rather than the window; and toasts are for
 * events while dialogs are for decisions.
 */
(function (global) {
    'use strict';

    const $ = (id) => document.getElementById(id);
    const overlay = () => $('dialog-overlay');

    function escapeHtml(text) {
        return String(text == null ? '' : text)
            .replace(/&/g, '&amp;')
            .replace(/</g, '&lt;')
            .replace(/>/g, '&gt;')
            .replace(/"/g, '&quot;')
            .replace(/'/g, '&#39;');
    }

    function el(tag, className, text) {
        const node = document.createElement(tag);
        if (className) node.className = className;
        if (text != null) node.textContent = text;
        return node;
    }

    // -----------------------------------------------------------------------
    // Dialogs
    // -----------------------------------------------------------------------

    let openDialogId = null;

    function openDialog(id) {
        const dialog = $(id);
        if (!dialog) return;
        for (const other of overlay().querySelectorAll('.dialog')) other.classList.remove('active');
        dialog.classList.add('active');
        overlay().classList.add('active');
        openDialogId = id;
        const focusable = dialog.querySelector('input, textarea, select, button.btn-primary');
        if (focusable) setTimeout(() => focusable.focus(), 30);
    }

    function closeDialog() {
        overlay().classList.remove('active');
        for (const dialog of overlay().querySelectorAll('.dialog')) dialog.classList.remove('active');
        openDialogId = null;
    }

    function isDialogOpen(id) {
        return id ? openDialogId === id : openDialogId !== null;
    }

    // -----------------------------------------------------------------------
    // Message dialog: alert / confirm / prompt / select, without a native one
    // -----------------------------------------------------------------------

    let messageResolver = null;

    function messageDialog(options) {
        const {
            title = 'Folio',
            text = '',
            kind = 'alert',
            value = '',
            placeholder = '',
            choices = [],
            okLabel = 'OK',
            cancelLabel = 'Cancel',
            danger = false,
        } = options || {};

        const dialogOverlay = $('message-dialog-overlay');
        $('message-dialog-title').textContent = title;
        $('message-dialog-text').textContent = text;

        const input = $('message-dialog-input');
        const textarea = $('message-dialog-textarea');
        const select = $('message-dialog-select');
        input.classList.remove('active');
        textarea.classList.remove('active');
        select.classList.remove('active');
        input.value = '';
        textarea.value = '';
        select.innerHTML = '';

        if (kind === 'prompt') {
            input.classList.add('active');
            input.value = value;
            input.placeholder = placeholder;
        } else if (kind === 'note') {
            textarea.classList.add('active');
            textarea.value = value;
            textarea.placeholder = placeholder;
        } else if (kind === 'select') {
            select.classList.add('active');
            for (const choice of choices) {
                const option = document.createElement('option');
                option.value = choice.value;
                option.textContent = choice.label;
                select.appendChild(option);
            }
            select.value = value || (choices[0] && choices[0].value) || '';
        }

        const ok = $('message-dialog-ok-btn');
        const cancel = $('message-dialog-cancel-btn');
        ok.textContent = okLabel;
        cancel.textContent = cancelLabel;
        cancel.style.display = kind === 'alert' ? 'none' : '';
        ok.classList.toggle('btn-danger', !!danger);

        dialogOverlay.classList.add('active');
        setTimeout(() => {
            if (kind === 'prompt') input.focus();
            else if (kind === 'note') textarea.focus();
            else if (kind === 'select') select.focus();
            else ok.focus();
        }, 30);

        return new Promise((resolve) => {
            messageResolver = (result) => {
                dialogOverlay.classList.remove('active');
                messageResolver = null;
                resolve(result);
            };
        });
    }

    function currentMessageValue() {
        const input = $('message-dialog-input');
        const textarea = $('message-dialog-textarea');
        const select = $('message-dialog-select');
        if (input.classList.contains('active')) return input.value;
        if (textarea.classList.contains('active')) return textarea.value;
        if (select.classList.contains('active')) return select.value;
        return true;
    }

    const UI = {
        $, el, escapeHtml,
        openDialog, closeDialog, isDialogOpen,

        alert: (text, title) => messageDialog({ text, title: title || 'Folio', kind: 'alert' }),

        confirm: (text, options) =>
            messageDialog(Object.assign({ text, kind: 'confirm' }, options)).then((v) => v !== null),

        prompt: (text, options) => messageDialog(Object.assign({ text, kind: 'prompt' }, options)),

        /** A multi-line prompt, for rejection notes and comment bodies. */
        note: (text, options) => messageDialog(Object.assign({ text, kind: 'note' }, options)),

        select: (text, choices, options) =>
            messageDialog(Object.assign({ text, kind: 'select', choices }, options)),

        /** Report an error from the core in the terms the core used. */
        error(e, context) {
            const message = e && e.message ? e.message : String(e);
            const title = context || 'Folio';
            console.error('folio:', context || '', e);
            return messageDialog({ title, text: message, kind: 'alert' });
        },

        // -------------------------------------------------------------------
        // Toasts: for events. Decisions get a dialog.
        // -------------------------------------------------------------------

        toast(message, options) {
            const { type = 'info', hint = '', timeout = 6000, action = null } = options || {};
            const container = $('toast-container');
            if (!container) return;

            const node = el('div', 'toast toast--' + type);
            const body = el('div', 'toast__body');
            body.appendChild(el('div', null, message));
            if (hint) body.appendChild(el('span', 'toast__hint', hint));
            if (action) {
                const button = el('button', 'toast__action', action.label);
                button.addEventListener('click', () => {
                    dismiss();
                    action.run();
                });
                body.appendChild(button);
            }
            node.appendChild(body);

            const close = el('button', 'toast__close', '×');
            close.setAttribute('aria-label', 'Dismiss');
            node.appendChild(close);

            let timer = null;
            function dismiss() {
                if (timer) clearTimeout(timer);
                node.classList.add('toast-hiding');
                setTimeout(() => node.remove(), 300);
            }
            close.addEventListener('click', dismiss);
            container.appendChild(node);
            if (timeout > 0) timer = setTimeout(dismiss, timeout);
            return dismiss;
        },

        loading(on, text) {
            const node = $('loading-overlay');
            if (!node) return;
            if (text) $('loading-text').textContent = text;
            node.classList.toggle('active', !!on);
        },

        status(text) {
            const node = $('status-text');
            if (node) node.textContent = text || '';
        },

        // -------------------------------------------------------------------
        // Menus
        // -------------------------------------------------------------------

        /** Wire the menu bar: click to open, hover to switch, click away to close. */
        initMenus(onAction) {
            const bar = $('menu-bar');
            if (!bar) return;

            const closeAll = () => {
                for (const category of bar.querySelectorAll('.menu-category')) {
                    category.classList.remove('active');
                }
            };

            for (const category of bar.querySelectorAll('.menu-category')) {
                const button = category.querySelector('.menu-category-btn');
                button.addEventListener('click', (event) => {
                    event.stopPropagation();
                    const wasOpen = category.classList.contains('active');
                    closeAll();
                    if (!wasOpen) category.classList.add('active');
                });
                category.addEventListener('mouseenter', () => {
                    if (bar.querySelector('.menu-category.active')) {
                        closeAll();
                        category.classList.add('active');
                    }
                });
            }

            bar.addEventListener('click', (event) => {
                const item = event.target.closest('.menu-item');
                if (!item || item.hasAttribute('disabled')) return;
                closeAll();
                onAction(item.dataset.action, item);
            });

            document.addEventListener('click', closeAll);
            document.addEventListener('keydown', (event) => {
                if (event.key === 'Escape') closeAll();
            });
        },

        /** Enable or disable a menu item by its action name. */
        setMenuEnabled(action, enabled) {
            for (const item of document.querySelectorAll('.menu-item[data-action="' + action + '"]')) {
                if (enabled) item.removeAttribute('disabled');
                else item.setAttribute('disabled', 'disabled');
            }
        },

        // -------------------------------------------------------------------
        // Splitters
        // -------------------------------------------------------------------

        /**
         * Drag-to-resize. `axis` is 'x' (a column splitter sizing the pane
         * before it) or 'y' (a row splitter sizing the pane after it).
         */
        initSplitter(splitterId, targetId, axis, options) {
            const splitter = $(splitterId);
            const target = $(targetId);
            if (!splitter || !target) return;
            const { min = 140, max = 0.85, storageKey = null, onResize = null } = options || {};

            if (storageKey) {
                try {
                    const saved = parseFloat(localStorage.getItem(storageKey));
                    if (saved > 0) applySize(saved);
                } catch (e) { /* private mode */ }
            }

            function applySize(px) {
                if (axis === 'x') target.style.flex = '0 0 ' + px + 'px';
                else target.style.flex = '0 0 ' + px + 'px';
                if (onResize) onResize(px);
            }

            let dragging = false;

            splitter.addEventListener('mousedown', (event) => {
                dragging = true;
                splitter.classList.add('dragging');
                document.body.style.cursor = axis === 'x' ? 'col-resize' : 'row-resize';
                document.body.style.userSelect = 'none';
                event.preventDefault();
            });

            document.addEventListener('mousemove', (event) => {
                if (!dragging) return;
                const container = target.parentElement.getBoundingClientRect();
                let size;
                if (axis === 'x') {
                    size = event.clientX - target.getBoundingClientRect().left;
                    size = Math.max(min, Math.min(size, container.width * max));
                } else {
                    size = target.getBoundingClientRect().bottom - event.clientY;
                    size = Math.max(min, Math.min(size, container.height * max));
                }
                applySize(Math.round(size));
            });

            document.addEventListener('mouseup', () => {
                if (!dragging) return;
                dragging = false;
                splitter.classList.remove('dragging');
                document.body.style.cursor = '';
                document.body.style.userSelect = '';
                if (storageKey) {
                    const rect = target.getBoundingClientRect();
                    try {
                        localStorage.setItem(storageKey, String(Math.round(axis === 'x' ? rect.width : rect.height)));
                    } catch (e) { /* private mode */ }
                }
            });
        },

        // -------------------------------------------------------------------
        // Formatting
        // -------------------------------------------------------------------

        /** "3 minutes ago" for anything recent, an absolute time beyond a week. */
        relativeTime(iso) {
            if (!iso) return '';
            const then = new Date(iso).getTime();
            if (Number.isNaN(then)) return '';
            const seconds = Math.round((Date.now() - then) / 1000);
            if (seconds < 45) return 'just now';
            if (seconds < 90) return 'a minute ago';
            const minutes = Math.round(seconds / 60);
            if (minutes < 60) return minutes + ' minutes ago';
            const hours = Math.round(minutes / 60);
            if (hours < 24) return hours === 1 ? 'an hour ago' : hours + ' hours ago';
            const days = Math.round(hours / 24);
            if (days < 7) return days === 1 ? 'yesterday' : days + ' days ago';
            return new Date(then).toLocaleDateString(undefined, {
                year: 'numeric', month: 'short', day: 'numeric',
            });
        },

        absoluteTime(iso) {
            if (!iso) return '';
            const date = new Date(iso);
            if (Number.isNaN(date.getTime())) return iso;
            return date.toLocaleString(undefined, {
                year: 'numeric', month: 'short', day: 'numeric',
                hour: '2-digit', minute: '2-digit',
            });
        },

        bytes(n) {
            if (n == null) return '';
            if (n < 1024) return n + ' B';
            if (n < 1024 * 1024) return (n / 1024).toFixed(1) + ' KB';
            if (n < 1024 * 1024 * 1024) return (n / (1024 * 1024)).toFixed(1) + ' MB';
            return (n / (1024 * 1024 * 1024)).toFixed(2) + ' GB';
        },

        delta(n) {
            if (n == null || n === 0) return '';
            return (n > 0 ? '+' : '') + n + 'B';
        },

        typeLabel(type) {
            return { skill: 'Skill', prompt: 'Prompt', task_list: 'Tasks', doc: 'Doc', asset: 'Asset' }[type] || type;
        },

        typeIcon(type) {
            return { skill: '◈', prompt: '▸', task_list: '☑', doc: '▤', asset: '▣' }[type] || '▤';
        },
    };

    // Wiring that belongs to the shell rather than to any one view.
    document.addEventListener('DOMContentLoaded', () => {
        overlay().addEventListener('click', (event) => {
            if (event.target === overlay()) closeDialog();
        });
        for (const button of document.querySelectorAll('[data-close-dialog]')) {
            button.addEventListener('click', closeDialog);
        }

        $('message-dialog-ok-btn').addEventListener('click', () => {
            if (messageResolver) messageResolver(currentMessageValue());
        });
        $('message-dialog-cancel-btn').addEventListener('click', () => {
            if (messageResolver) messageResolver(null);
        });
        $('message-dialog-input').addEventListener('keydown', (event) => {
            if (event.key === 'Enter' && messageResolver) messageResolver(currentMessageValue());
        });

        document.addEventListener('keydown', (event) => {
            if (event.key !== 'Escape') return;
            // The message dialog stacks above everything, so it closes first.
            if (messageResolver) {
                messageResolver(null);
                event.stopPropagation();
                return;
            }
            if (openDialogId) {
                closeDialog();
                event.stopPropagation();
            }
        }, true);
    });

    global.UI = UI;
})(window);
