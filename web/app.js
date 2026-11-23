(function(){
  "use strict";

  /* tabs — each [data-tabs] group is independent (a page can hold several) */
  Array.prototype.slice.call(document.querySelectorAll('[data-tabs]')).forEach(function(group){
    var tabs = Array.prototype.slice.call(group.querySelectorAll('[role="tab"]'));
    if (!tabs.length) return;
    function selectTab(tab){
      tabs.forEach(function(t){
        var sel = t === tab;
        t.setAttribute('aria-selected', sel ? 'true' : 'false');
        t.tabIndex = sel ? 0 : -1;
        var panel = document.getElementById(t.getAttribute('aria-controls'));
        if (panel) panel.hidden = !sel;
      });
    }
    tabs.forEach(function(t, i){
      t.tabIndex = t.getAttribute('aria-selected') === 'true' ? 0 : -1;
      t.addEventListener('click', function(){ selectTab(t); });
      t.addEventListener('keydown', function(e){
        var n = null;
        if (e.key === 'ArrowRight') n = tabs[(i + 1) % tabs.length];
        else if (e.key === 'ArrowLeft') n = tabs[(i - 1 + tabs.length) % tabs.length];
        else if (e.key === 'Home') n = tabs[0];
        else if (e.key === 'End') n = tabs[tabs.length - 1];
        if (!n) return;
        e.preventDefault();
        selectTab(n); n.focus();
      });
    });
  });

  /* copy buttons */
  var copyStatus = document.getElementById('copy-status');
  document.querySelectorAll('.copy').forEach(function(btn){
    btn.addEventListener('click', function(){
      var text = btn.getAttribute('data-copy');
      var done = function(){
        var old = btn.textContent;
        btn.textContent = 'copiado';
        if (copyStatus) copyStatus.textContent = 'Comando copiado para a área de transferência.';
        setTimeout(function(){ btn.textContent = old; }, 1400);
      };
      if (navigator.clipboard && navigator.clipboard.writeText) {
        navigator.clipboard.writeText(text).then(done, function(){});
      } else {
        var ta = document.createElement('textarea');
        ta.value = text; document.body.appendChild(ta); ta.select();
        try { document.execCommand('copy'); done(); } catch(e){}
        document.body.removeChild(ta);
      }
    });
  });

  /* translation probe — switches the pair of examples shown */
  var PROBES = {
    text: {
      messages: ['turnos <code>user</code> / <code>assistant</code> separados', 'lista <code>messages[]</code> com <code>role</code> por item'],
      system:   ['parâmetro <code>system</code> de nível superior', 'primeira entrada com <code>role: "system"</code>'],
      images:   ['bloco <code>{"type":"image"}</code> com base64 ou URL', 'parte <code>image_url</code> na mensagem'],
      max:      ['<code>max_tokens</code>, obrigatório', '<code>max_tokens</code>, opcional'],
      stop:     ['<code>stop_sequences[]</code>', '<code>stop[]</code>'],
      ret:      ['<code>"type": "message"</code>', 'escolha com <code>finish_reason: "stop"</code>']
    },
    tool: {
      messages: ['histórico misto de texto e blocos', 'mensagens com <code>role</code> por item'],
      system:   ['parâmetro <code>system</code> de nível superior', 'primeira entrada com <code>role: "system"</code>'],
      tools:    ['<code>tools[]</code> com <code>input_schema</code>', '<code>tools[]</code> com <code>function.parameters</code>'],
      call:     ['bloco <code>tool_use</code> com <code>id</code>', '<code>tool_calls[]</code> com <code>id</code> em JSON'],
      result:   ['bloco <code>tool_result</code> do usuário', 'mensagem <code>role: "tool"</code>'],
      ret:      ['<code>stop_reason: "tool_use"</code>', '<code>finish_reason: "tool_calls"</code>']
    },
    stream: {
      messages: ['turnos <code>user</code> / <code>assistant</code> separados', 'lista <code>messages[]</code> com <code>role</code> por item'],
      events:   ['<code>message_start</code> / <code>content_block_delta</code>', 'deltas do provedor, quando existirem'],
      partial:  ['blocos parciais com <code>index</code>', 'pedaços de <code>choices[0].delta</code>'],
      end:      ['<code>message_delta</code> com <code>stop_reason</code>', '<code>[DONE]</code> ou fim do stream'],
      usage:    ['<code>input_tokens</code> / <code>output_tokens</code>', '<code>prompt_tokens</code> / <code>completion_tokens</code>'],
      ret:      ['eventos SSE no dialeto da Anthropic', 'formato nativo do provedor']
    }
  };

  var tbody = document.querySelector('.translate tbody');
  var probes = document.querySelectorAll('[data-probe]');
  var probeStatus = document.getElementById('probe-status');
  var PROBE_LABELS = { text: 'texto', tool: 'tool use', stream: 'streaming' };
  function renderProbe(name){
    var spec = PROBES[name];
    if (!spec || !tbody) return;
    tbody.innerHTML = Object.keys(spec).map(function(k){
      return '<tr><td>' + k + '</td><td data-a>' + spec[k][0] + '</td><td data-b>' + spec[k][1] + '</td></tr>';
    }).join('');
    probes.forEach(function(p){ p.setAttribute('aria-pressed', String(p.getAttribute('data-probe') === name)); });
    if (probeStatus) probeStatus.textContent = 'Exibindo exemplo de ' + (PROBE_LABELS[name] || name) + '.';
  }
  probes.forEach(function(p){
    p.addEventListener('click', function(){ renderProbe(p.getAttribute('data-probe')); });
  });

  /* docs sidebar — mark the current page and the section in view */
  /* docs sidebar — toggle on narrow screens, then mark page/section */
  var toc = document.getElementById('toc');
  var navToggle = document.querySelector('.docs-nav-toggle');
  if (navToggle && toc) {
    navToggle.addEventListener('click', function(){
      var open = toc.classList.toggle('open');
      navToggle.setAttribute('aria-expanded', String(open));
    });
  }

  if (toc) {
    var page = location.pathname.split('/').pop() || 'index.html';
    var links = Array.prototype.slice.call(toc.querySelectorAll('a'));
    links.forEach(function(a){
      var target = a.getAttribute('href') || '';
      if (target === page) a.setAttribute('aria-current', 'page');
      if (target.charAt(0) === '#' && target === location.hash) a.classList.add('on');
    });

    // Hash links inside the same page: highlight as they scroll past.
    var local = links.filter(function(a){ return (a.getAttribute('href') || '').charAt(0) === '#'; });
    if (local.length && 'IntersectionObserver' in window) {
      var byId = {};
      local.forEach(function(a){ byId[a.getAttribute('href').slice(1)] = a; });
      var io = new IntersectionObserver(function(entries){
        entries.forEach(function(e){
          if (!e.isIntersecting) return;
          local.forEach(function(a){ a.classList.remove('on'); });
          var active = byId[e.target.id];
          if (active) active.classList.add('on');
        });
      }, { rootMargin: '-15% 0px -70% 0px', threshold: 0 });
      Object.keys(byId).forEach(function(id){
        var el = document.getElementById(id);
        if (el) io.observe(el);
      });
    }
  }
})();
