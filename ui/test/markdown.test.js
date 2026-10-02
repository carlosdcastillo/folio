const assert = require('node:assert/strict');
const test = require('node:test');

global.marked = require('../lib/marked.min.js');
global.window = {
    UI: {
        escapeHtml(value) {
            return String(value)
                .replaceAll('&', '&amp;')
                .replaceAll('<', '&lt;')
                .replaceAll('>', '&gt;')
                .replaceAll('"', '&quot;')
                .replaceAll("'", '&#39;');
        },
    },
};

require('../js/markdown.js');

function render(source) {
    return global.marked.parse(source).trim();
}

test('approximation tildes remain literal', () => {
    const source = '~46 to 86 SDE-weeks (~$0.5M to 0.85M)';
    const html = render(source);

    assert.equal(html, `<p>${source}</p>`);
    assert.doesNotMatch(html, /<del>/);
});

test('single-tilde pairs remain literal', () => {
    assert.equal(render('about ~46 weeks'), '<p>about ~46 weeks</p>');
    assert.equal(render('~one~'), '<p>~one~</p>');
    assert.equal(render('~**important**~'), '<p>~<strong>important</strong>~</p>');
});

test('double tildes still produce intentional strikethrough', () => {
    assert.equal(render('~~obsolete~~'), '<p><del>obsolete</del></p>');
});

test('escaped tildes and code retain their markdown meaning', () => {
    assert.equal(render('\\~literal\\~'), '<p>~literal~</p>');
    assert.equal(render('`~code~`'), '<p><code>~code~</code></p>');
    assert.equal(render('```text\n~code~\n```'), '<pre><code class="language-text">~code~\n</code></pre>');
});
