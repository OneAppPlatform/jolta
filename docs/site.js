// Tiny shell highlighter: first token = command, next word = subcommand,
// -flags, pipes, and # comments. The hand-annotated .demo block is skipped.
document.querySelectorAll('main pre:not(.demo) > code').forEach(function (code) {
  var esc = function (s) {
    return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
  };
  code.innerHTML = code.textContent.split('\n').map(function (line) {
    var comment = '';
    var ci = line.indexOf('#');
    if (ci === 0 || (ci > 0 && line[ci - 1] === ' ')) {
      comment = '<span class="tok-com">' + esc(line.slice(ci)) + '</span>';
      line = line.slice(0, ci);
    }
    var pos = 0; // 0 = expecting command, 1 = expecting subcommand, 2 = args
    return line.split(/(\s+)/).map(function (t) {
      if (!t.trim()) return t;
      if (t === '|' || t === '&&' || t === ';') { pos = 0; return '<span class="tok-op">' + esc(t) + '</span>'; }
      if (pos === 0) { pos = 1; return '<span class="tok-cmd">' + esc(t) + '</span>'; }
      if (t[0] === '-') return '<span class="tok-flag">' + esc(t) + '</span>';
      if (pos === 1) { pos = 2; if (/^[a-z][a-z-]*$/.test(t)) return '<span class="tok-sub">' + esc(t) + '</span>'; }
      return esc(t);
    }).join('') + comment;
  }).join('\n');
});

// Copy-to-clipboard buttons on every command block (the illustrative
// .demo transcript is skipped — its $-prompts aren't paste-able).
(function () {
  var COPY = '<svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true"><path d="M0 6.75C0 5.784.784 5 1.75 5h1.5a.75.75 0 0 1 0 1.5h-1.5a.25.25 0 0 0-.25.25v7.5c0 .138.112.25.25.25h7.5a.25.25 0 0 0 .25-.25v-1.5a.75.75 0 0 1 1.5 0v1.5A1.75 1.75 0 0 1 9.25 16h-7.5A1.75 1.75 0 0 1 0 14.25Z"/><path d="M5 1.75C5 .784 5.784 0 6.75 0h7.5C15.216 0 16 .784 16 1.75v7.5A1.75 1.75 0 0 1 14.25 11h-7.5A1.75 1.75 0 0 1 5 9.25Zm1.75-.25a.25.25 0 0 0-.25.25v7.5c0 .138.112.25.25.25h7.5a.25.25 0 0 0 .25-.25v-7.5a.25.25 0 0 0-.25-.25Z"/></svg>';
  var CHECK = '<svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true"><path d="M13.78 4.22a.75.75 0 0 1 0 1.06l-7.25 7.25a.75.75 0 0 1-1.06 0L2.22 9.28a.75.75 0 0 1 1.06-1.06L6 10.94l6.72-6.72a.75.75 0 0 1 1.06 0Z"/></svg>';
  document.querySelectorAll('main pre:not(.demo)').forEach(function (pre) {
    var code = pre.querySelector('code');
    if (!code) return;
    var wrap = document.createElement('div');
    wrap.className = 'codewrap';
    pre.parentNode.insertBefore(wrap, pre);
    wrap.appendChild(pre);
    var btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'copy';
    btn.title = 'Copy to clipboard';
    btn.setAttribute('aria-label', 'Copy to clipboard');
    btn.innerHTML = COPY;
    btn.addEventListener('click', function () {
      var text = code.textContent.trim() + '\n';
      var fallback = function () {
        var ta = document.createElement('textarea');
        ta.value = text;
        ta.style.position = 'fixed';
        ta.style.opacity = '0';
        document.body.appendChild(ta);
        ta.select();
        var ok = false;
        try { ok = document.execCommand('copy'); } catch (e) { /* leave ok false */ }
        document.body.removeChild(ta);
        return ok ? Promise.resolve() : Promise.reject();
      };
      var write = navigator.clipboard && navigator.clipboard.writeText
        ? navigator.clipboard.writeText(text).catch(fallback)
        : fallback();
      write.then(function () {
        btn.innerHTML = CHECK;
        btn.classList.add('ok');
        setTimeout(function () { btn.innerHTML = COPY; btn.classList.remove('ok'); }, 1500);
      });
    });
    wrap.appendChild(btn);
  });
})();
