/*
 * Broodlink — Model Manager
 * List, pull (with real SSE progress), delete Ollama models.
 */
(function () {
  'use strict';

  var BL = window.Broodlink;
  var tbody = document.getElementById('models-tbody');
  if (!tbody) return;

  var installedNames = {};

  // -- Load & render installed models --

  function loadModels(cb) {
    fetch('/api/v1/models')
      .then(function (r) { return r.json(); })
      .then(function (data) {
        var models = data.models || [];
        installedNames = {};
        models.forEach(function (m) { installedNames[m.name] = true; });
        renderModels(models);
        updateMetrics(models);
        if (cb) cb();
      })
      .catch(function () {
        tbody.innerHTML = '<tr><td colspan="5">Failed to load models. Is Ollama running?</td></tr>';
      });
  }

  function renderModels(models) {
    if (models.length === 0) {
      tbody.innerHTML = '<tr><td colspan="5" class="loading-cell">No models installed. Click "Download New Model" to get started.</td></tr>';
      return;
    }

    tbody.innerHTML = models.map(function (m) {
      var roleClass = m.role ? ' ' + m.role : '';
      var roleLabel = m.role || 'general';
      var modified = m.modified ? BL.formatRelativeTime(m.modified) : '—';

      return '<tr>' +
        '<td><strong>' + BL.escapeHtml(m.name) + '</strong></td>' +
        '<td><span class="role-badge' + roleClass + '">' + roleLabel + '</span></td>' +
        '<td>' + m.size + '</td>' +
        '<td>' + modified + '</td>' +
        '<td>' +
          '<button class="btn btn-sm btn-danger delete-btn" data-name="' + BL.escapeHtml(m.name) + '">Remove</button>' +
        '</td>' +
        '</tr>';
    }).join('');

    var btns = document.querySelectorAll('.delete-btn');
    for (var i = 0; i < btns.length; i++) {
      btns[i].addEventListener('click', function () {
        var name = this.getAttribute('data-name');
        if (confirm('Remove ' + name + '? You can re-download it later.')) {
          deleteModel(name);
        }
      });
    }
  }

  function updateMetrics(models) {
    document.getElementById('metric-total').textContent = models.length;
    var totalBytes = 0;
    var roles = {};
    models.forEach(function (m) {
      totalBytes += m.size_bytes || 0;
      if (m.role) roles[m.role] = true;
    });
    var gb = (totalBytes / 1073741824).toFixed(1);
    document.getElementById('metric-size').textContent = gb + ' GB';
    document.getElementById('metric-roles').textContent = Object.keys(roles).length;
  }

  // -- Pull model with real SSE progress --

  var currentPull = null;

  function pullModel(name, sourceCard) {
    if (currentPull) {
      alert('A download is already in progress. Please wait.');
      return;
    }
    currentPull = name;

    var progressEl = document.getElementById('pull-progress');
    var resultEl = document.getElementById('pull-result');
    var nameEl = document.getElementById('pull-progress-name');
    var pctEl = document.getElementById('pull-progress-pct');
    var fillEl = document.getElementById('pull-progress-fill');
    var statusEl = document.getElementById('pull-progress-status');

    resultEl.style.display = 'none';
    progressEl.style.display = 'block';
    nameEl.textContent = name;
    pctEl.textContent = '0%';
    fillEl.style.width = '0%';
    statusEl.textContent = 'Connecting to Ollama...';

    if (sourceCard) sourceCard.classList.add('downloading');

    // Use SSE for real progress
    var url = '/api/v1/models/pull/progress?name=' + encodeURIComponent(name);
    var es = new EventSource(url);
    var lastPct = 0;

    es.onmessage = function (e) {
      try {
        var d = JSON.parse(e.data);

        if (d.error) {
          es.close();
          showResult('error', 'Failed: ' + d.error);
          finish(sourceCard);
          return;
        }

        if (d.status === 'done') {
          es.close();
          fillEl.style.width = '100%';
          pctEl.textContent = '100%';
          statusEl.textContent = 'Complete';
          showResult('success', BL.escapeHtml(name) + ' downloaded successfully!');
          loadModels(function () { markInstalledCards(); });
          finish(sourceCard);
          return;
        }

        // Update progress
        var pct = d.percent || 0;
        if (pct > lastPct) lastPct = pct;
        fillEl.style.width = lastPct + '%';
        pctEl.textContent = lastPct + '%';

        var detail = d.status || '';
        if (d.total > 0) {
          detail += ' (' + formatBytes(d.completed) + ' / ' + formatBytes(d.total) + ')';
        }
        statusEl.textContent = detail;
      } catch (err) { /* ignore parse errors */ }
    };

    es.onerror = function () {
      es.close();
      // If we never got progress, it might be a connection error
      if (lastPct === 0) {
        showResult('error', 'Lost connection to download stream. Check if Ollama is running.');
      } else {
        // SSE can close normally when done — check if model appeared
        loadModels(function () {
          if (installedNames[name]) {
            fillEl.style.width = '100%';
            pctEl.textContent = '100%';
            showResult('success', BL.escapeHtml(name) + ' downloaded successfully!');
          }
          markInstalledCards();
        });
      }
      finish(sourceCard);
    };
  }

  function finish(sourceCard) {
    currentPull = null;
    if (sourceCard) sourceCard.classList.remove('downloading');
  }

  function showResult(type, msg) {
    var el = document.getElementById('pull-result');
    el.style.display = 'block';
    el.innerHTML = '<div class="alert alert-' + type + '">' + msg + '</div>';
  }

  function formatBytes(bytes) {
    if (!bytes) return '0 B';
    if (bytes >= 1073741824) return (bytes / 1073741824).toFixed(1) + ' GB';
    if (bytes >= 1048576) return (bytes / 1048576).toFixed(0) + ' MB';
    return (bytes / 1024).toFixed(0) + ' KB';
  }

  // -- Delete model --

  function deleteModel(name) {
    fetch('/api/v1/models/' + encodeURIComponent(name), { method: 'DELETE' })
      .then(function (r) { return r.json(); })
      .then(function () {
        loadModels(function () { markInstalledCards(); });
      })
      .catch(function (err) {
        alert('Failed to remove: ' + (err.message || 'unknown'));
      });
  }

  // -- Recommended models --

  var recModels = [];

  function loadRecommended() {
    fetch('/api/v1/models/recommended')
      .then(function (r) { return r.json(); })
      .then(function (models) {
        recModels = models;
        renderRecommended();
      });
  }

  function renderRecommended() {
    var coreEl = document.getElementById('rec-core');
    var advEl = document.getElementById('rec-advanced');
    if (!coreEl || !advEl) return;

    coreEl.innerHTML = '';
    advEl.innerHTML = '';

    recModels.forEach(function (m) {
      var isInstalled = !!installedNames[m.name];
      var container = m.category === 'core' ? coreEl : advEl;

      var card = document.createElement('div');
      card.className = 'rec-card' + (isInstalled ? ' installed' : '');
      card.setAttribute('data-name', m.name);

      card.innerHTML =
        '<span class="role-badge ' + BL.escapeHtml(m.role) + '">' + BL.escapeHtml(m.role) + '</span>' +
        '<div class="rec-card-info">' +
          '<div class="rec-card-name">' + BL.escapeHtml(m.name) + '</div>' +
          '<div class="rec-card-desc">' + BL.escapeHtml(m.description) + '</div>' +
        '</div>' +
        '<div class="rec-card-meta">' +
          '<div>' + BL.escapeHtml(m.size) + '</div>' +
          '<div class="rec-card-status' + (isInstalled ? ' ok' : '') + '">' +
            (isInstalled ? 'Installed' : 'Click to download') +
          '</div>' +
        '</div>';

      if (!isInstalled) {
        card.addEventListener('click', function () {
          pullModel(m.name, card);
        });
      }

      container.appendChild(card);
    });
  }

  function markInstalledCards() {
    renderRecommended();
  }

  // -- Tabs --

  var tabs = document.querySelectorAll('.pull-tab');
  for (var t = 0; t < tabs.length; t++) {
    tabs[t].addEventListener('click', function () {
      var target = this.getAttribute('data-tab');
      for (var j = 0; j < tabs.length; j++) {
        tabs[j].classList.remove('active');
        tabs[j].setAttribute('aria-selected', 'false');
      }
      this.classList.add('active');
      this.setAttribute('aria-selected', 'true');
      document.getElementById('tab-recommended').style.display = target === 'recommended' ? 'block' : 'none';
      document.getElementById('tab-custom').style.display = target === 'custom' ? 'block' : 'none';
    });
  }

  // -- Modal --

  var modal = document.getElementById('pull-modal');

  document.getElementById('show-pull-btn').addEventListener('click', function () {
    modal.style.display = 'block';
    document.getElementById('pull-progress').style.display = 'none';
    document.getElementById('pull-result').style.display = 'none';
    loadRecommended();
  });

  document.getElementById('close-pull-modal').addEventListener('click', function () {
    modal.style.display = 'none';
  });

  modal.addEventListener('click', function (e) {
    if (e.target === modal) modal.style.display = 'none';
  });

  document.getElementById('pull-form').addEventListener('submit', function (e) {
    e.preventDefault();
    var name = document.getElementById('pull-name').value.trim();
    if (name) pullModel(name, null);
  });

  document.getElementById('refresh-btn').addEventListener('click', function () { loadModels(); });

  // -- Init --
  loadModels();
  setInterval(loadModels, 30000);
})();
