/* The frontend's whole view of the backend.
 *
 * There is exactly one call — `Folio.call(op, params)` — because there is
 * exactly one dispatch table in the core, and the MCP tools go through the same
 * one. If the UI can do it, an agent can too, and vice versa.
 */
(function (global) {
    'use strict';

    const tauri = global.__TAURI__ || null;
    const invoke = tauri && tauri.core ? tauri.core.invoke : null;
    let platform = document.documentElement.dataset.platform || 'linux';

    function formatShortcut(value) {
        const parts = String(value || '').split('+');
        if (platform === 'macos') {
            const symbols = { Mod: '⌘', Shift: '⇧', Alt: '⌥', Enter: '↩' };
            return parts.map((part) => symbols[part] || part.toUpperCase()).join('');
        }
        return parts.map((part) => part === 'Mod' ? 'Ctrl' : part).join('+');
    }

    function applyPlatform(value) {
        platform = value === 'macos' || value === 'windows' ? value : 'linux';
        document.documentElement.dataset.platform = platform;
        for (const node of document.querySelectorAll('[data-shortcut]')) {
            const shortcut = platform === 'macos' && node.dataset.shortcutMacos
                ? node.dataset.shortcutMacos : node.dataset.shortcut;
            if (node.dataset.shortcutTitle) {
                node.title = node.dataset.shortcutTitle.replace('{shortcut}', formatShortcut(shortcut));
            } else {
                node.textContent = formatShortcut(shortcut);
            }
        }
        for (const node of document.querySelectorAll('[data-platform-label]')) {
            node.textContent = platform === 'macos' ? node.dataset.macos : node.dataset.default;
        }
    }

    class FolioError extends Error {
        constructor(wire) {
            super((wire && wire.message) || 'Something went wrong.');
            this.name = 'FolioError';
            this.code = (wire && wire.code) || 'error';
            this.data = wire ? wire.data : undefined;
        }
    }

    /** Tauri rejects with whatever the command returned; normalise it. */
    function normalizeError(raw) {
        if (raw && typeof raw === 'object' && raw.code) return raw;
        if (raw instanceof Error) return { code: 'error', message: raw.message };
        if (typeof raw === 'string') return { code: 'error', message: raw };
        return { code: 'error', message: String(raw) };
    }

    const listeners = new Map();

    function dispatchEvent(event) {
        if (!event || !event.type) return;
        for (const type of [event.type, '*']) {
            const set = listeners.get(type);
            if (!set) continue;
            for (const fn of Array.from(set)) {
                try {
                    fn(event);
                } catch (e) {
                    console.error('folio: event listener failed', e);
                }
            }
        }
    }

    const Folio = {
        FolioError,

        /** True when running inside the desktop shell rather than a plain browser. */
        get embedded() {
            return !!invoke;
        },

        shortcut: formatShortcut,
        applyPlatform,

        async call(op, params) {
            if (!invoke) {
                throw new FolioError({
                    code: 'no_backend',
                    message: 'Folio is running without its core. Launch the desktop app.',
                });
            }
            try {
                return await invoke('folio_call', { op, params: params || {} });
            } catch (raw) {
                throw new FolioError(normalizeError(raw));
            }
        },

        /** Like `call`, but returns `fallback` instead of throwing. */
        async tryCall(op, params, fallback) {
            try {
                return await this.call(op, params);
            } catch (e) {
                console.warn('folio: ' + op + ' failed', e);
                return fallback;
            }
        },

        async boot() {
            if (!invoke) return { version: 'dev', store_dir: '(no core)', platform: 'web' };
            const facts = await invoke('folio_boot');
            applyPlatform(facts.platform);
            return facts;
        },

        /** Open an external HTTP(S) link in the user's default browser. */
        async openUrl(url) {
            if (!tauri || !tauri.opener) return false;
            await tauri.opener.openUrl(url);
            return true;
        },

        /** Subscribe to a core event type, or '*' for all of them. */
        on(type, fn) {
            if (!listeners.has(type)) listeners.set(type, new Set());
            listeners.get(type).add(fn);
            return () => listeners.get(type).delete(fn);
        },

        /** Used by the app to fan out synthetic events (e.g. after a local save). */
        emit(event) {
            dispatchEvent(event);
        },

        /** Window controls, so the title bar works without native decorations. */
        window: {
            async minimize() {
                if (tauri && tauri.window) await tauri.window.getCurrentWindow().minimize();
            },
            async toggleMaximize() {
                if (tauri && tauri.window) await tauri.window.getCurrentWindow().toggleMaximize();
            },
            async close() {
                if (tauri && tauri.window) await tauri.window.getCurrentWindow().close();
            },
            async setTitle(title) {
                if (tauri && tauri.window) await tauri.window.getCurrentWindow().setTitle(title);
            },
            async show() {
                if (tauri && tauri.window) await tauri.window.getCurrentWindow().show();
            },
        },

        async copyToClipboard(text) {
            try {
                await navigator.clipboard.writeText(text);
                return true;
            } catch (e) {
                // WebView2 can refuse the async clipboard without a user
                // gesture; the legacy path still works there.
                try {
                    const helper = document.createElement('textarea');
                    helper.value = text;
                    helper.style.position = 'fixed';
                    helper.style.opacity = '0';
                    document.body.appendChild(helper);
                    helper.select();
                    const ok = document.execCommand('copy');
                    document.body.removeChild(helper);
                    return ok;
                } catch (inner) {
                    return false;
                }
            }
        },
    };

    if (tauri && tauri.event) {
        tauri.event.listen('folio://event', (message) => dispatchEvent(message.payload));
    }

    global.Folio = Folio;
})(window);
