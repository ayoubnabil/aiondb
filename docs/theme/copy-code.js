(function () {
  'use strict';

  function languageFor(pre) {
    for (var i = 0; i < pre.classList.length; i += 1) {
      var cls = pre.classList[i];
      if (cls.indexOf('lang-') === 0) return cls.slice(5);
    }
    return 'code';
  }

  function escapeHtml(text) {
    return text
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;');
  }

  function highlight(pre) {
    if (pre.dataset.highlighted === 'true') return;
    var lang = languageFor(pre);
    var code = pre.querySelector('code');
    if (!code) return;

    var source = code.textContent;
    var rendered = escapeHtml(source);

    if (lang === 'sql') {
      rendered = rendered
        .replace(/(--.*)$/gm, '<span class="code-token-comment">$1</span>')
        .replace(/('[^']*')/g, '<span class="code-token-string">$1</span>')
        .replace(/\b(SELECT|FROM|JOIN|ON|WHERE|ORDER BY|GROUP BY|LIMIT|CREATE|TABLE|INSERT|INTO|VALUES|AS|ASC|DESC|DROP|INDEX|USING|RETURN|MATCH|AND|OR|NOT|NULL|PRIMARY|KEY|BEGIN|COMMIT|ROLLBACK|SAVEPOINT|UPDATE|SET)\b/g, '<span class="code-token-key">$1</span>');
    } else if (lang === 'bash' || lang === 'sh' || lang === 'shell') {
      rendered = rendered
        .replace(/(#.*)$/gm, '<span class="code-token-comment">$1</span>')
        .replace(/(&quot;[^&]*?&quot;|'[^']*')/g, '<span class="code-token-string">$1</span>')
        .replace(/\b([A-Z][A-Z0-9_]*)(=)/g, '<span class="code-token-var">$1</span>$2')
        .replace(/(\$[{]?[A-ZA-Z0-9_]+[}]?)/g, '<span class="code-token-var">$1</span>')
        .replace(/(^|\s)(cargo|aiondb|docker|psql|source|cp|make|curl|export|BENCH_ENGINES)(?=\s|$)/gm, '$1<span class="code-token-command">$2</span>')
        .replace(/(^|\s)(-{1,2}[a-zA-Z0-9][a-zA-Z0-9_-]*)(?=\s|$)/gm, '$1<span class="code-token-flag">$2</span>')
        .replace(/\b([0-9]+(?:\.[0-9]+)?)(?=\b)/g, '<span class="code-token-number">$1</span>');
    } else {
      pre.dataset.highlighted = 'true';
      return;
    }

    code.innerHTML = rendered;
    pre.dataset.highlighted = 'true';
  }

  function writeClipboard(text) {
    if (navigator.clipboard && navigator.clipboard.writeText) {
      return navigator.clipboard.writeText(text);
    }
    var area = document.createElement('textarea');
    area.value = text;
    area.setAttribute('readonly', '');
    area.style.position = 'fixed';
    area.style.left = '-9999px';
    document.body.appendChild(area);
    area.select();
    try {
      document.execCommand('copy');
    } finally {
      document.body.removeChild(area);
    }
    return Promise.resolve();
  }

  function enhanceCodeBlocks(root) {
    var scope = root || document;
    var blocks = scope.querySelectorAll('.content pre:not(.hero-query)');

    blocks.forEach(function (pre) {
      if (pre.closest('.code-shell')) return;
      highlight(pre);

      var shell = document.createElement('div');
      shell.className = 'code-shell';
      pre.parentNode.insertBefore(shell, pre);
      shell.appendChild(pre);

      var toolbar = document.createElement('div');
      toolbar.className = 'code-toolbar';

      var label = document.createElement('span');
      label.className = 'code-lang';
      label.textContent = languageFor(pre);

      var button = document.createElement('button');
      button.type = 'button';
      button.className = 'code-copy';
      button.textContent = 'Copy';
      button.addEventListener('click', function () {
        var code = pre.querySelector('code');
        var text = code ? code.textContent : pre.textContent;
        writeClipboard(text).then(function () {
          button.textContent = 'Copied';
          window.setTimeout(function () {
            button.textContent = 'Copy';
          }, 1200);
        }).catch(function () {
          button.textContent = 'Failed';
          window.setTimeout(function () {
            button.textContent = 'Copy';
          }, 1200);
        });
      });

      toolbar.appendChild(label);
      toolbar.appendChild(button);
      shell.insertBefore(toolbar, pre);
    });
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', function () {
      enhanceCodeBlocks(document);
    });
  } else {
    enhanceCodeBlocks(document);
  }

  document.addEventListener('aion:navigated', function () {
    enhanceCodeBlocks(document);
  });
})();
