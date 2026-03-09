/*
 * Broodlink — Setup Wizard
 * Drives the first-run setup: deps → databases → models → launch.
 * All interaction via the web UI, no terminal needed.
 */
(function () {
  'use strict';

  var BL = window.Broodlink;
  var currentStep = 0;
  var systemState = null;

  var DEP_META = {
    ollama:   { label: 'Ollama',     purpose: 'AI model inference (runs your local models)' },
    dolt:     { label: 'Dolt',       purpose: 'Versioned agent memory database' },
    postgres: { label: 'PostgreSQL', purpose: 'Task queue, audit log, chat sessions' },
    nats:     { label: 'NATS',       purpose: 'Service-to-service messaging' },
    qdrant:   { label: 'Qdrant',     purpose: 'Vector search for memory recall' }
  };

  // -- Navigation --

  function goToStep(n) {
    currentStep = n;
    var steps = document.querySelectorAll('.setup-step');
    for (var i = 0; i < steps.length; i++) {
      steps[i].style.display = i === n ? 'block' : 'none';
    }
    var pills = document.querySelectorAll('.step-pill');
    for (var j = 0; j < pills.length; j++) {
      pills[j].className = 'step-pill' + (j === n ? ' active' : (j < n ? ' done' : ''));
    }
  }

  // -- Step 1: Dependencies --

  function renderDeps(state) {
    var tbody = document.getElementById('deps-tbody');
    var allOk = true;
    var rows = '';

    Object.keys(DEP_META).forEach(function (key) {
      var dep = state[key];
      var meta = DEP_META[key];
      var installed = dep && dep.installed;
      var running = dep && dep.running;
      var ok = installed && running;
      if (!ok) allOk = false;

      var statusHtml = ok
        ? '<span style="color:var(--green);">Ready</span>'
        : installed
          ? '<span style="color:var(--amber);">Installed, not running</span>'
          : '<span style="color:var(--red);">Not installed</span>';

      var actionHtml = ok
        ? '<span style="color:var(--text-secondary);">—</span>'
        : '<button class="btn btn-sm btn-primary install-btn" data-dep="' + key + '">' +
          (installed ? 'Start' : 'Install') + '</button>';

      rows += '<tr>' +
        '<td><strong>' + BL.escapeHtml(meta.label) + '</strong></td>' +
        '<td>' + meta.purpose + '</td>' +
        '<td>' + statusHtml + '</td>' +
        '<td>' + actionHtml + '</td>' +
        '</tr>';
    });

    tbody.innerHTML = rows;
    document.getElementById('next-1-btn').disabled = !allOk;
    document.getElementById('install-all-btn').disabled = allOk;

    // Bind install buttons
    var btns = document.querySelectorAll('.install-btn');
    for (var i = 0; i < btns.length; i++) {
      btns[i].addEventListener('click', function () {
        var dep = this.getAttribute('data-dep');
        installDep(dep, this);
      });
    }
  }

  function installDep(name, btn) {
    btn.disabled = true;
    btn.textContent = 'Installing...';

    fetch('/setup/install', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name: name })
    })
      .then(function (r) { return r.json(); })
      .then(function (data) {
        if (data.success) {
          btn.textContent = 'Done';
          btn.className = 'btn btn-sm';
          refreshStatus();
        } else {
          btn.textContent = 'Failed';
          btn.disabled = false;
          alert('Installation failed: ' + data.message);
        }
      })
      .catch(function () {
        btn.textContent = 'Error';
        btn.disabled = false;
      });
  }

  function installAllMissing() {
    var btn = document.getElementById('install-all-btn');
    btn.disabled = true;
    btn.textContent = 'Installing all...';

    fetch('/setup/install-all', { method: 'POST' })
      .then(function (r) { return r.json(); })
      .then(function (data) {
        if (data.success) {
          btn.textContent = 'Done';
        } else {
          btn.textContent = 'Some failed';
          alert('Issues: ' + data.message);
        }
        refreshStatus();
      })
      .catch(function () {
        btn.textContent = 'Install Missing';
        btn.disabled = false;
        alert('Installation request failed. Check your internet connection.');
      });
  }

  // -- Step 2: Databases --

  function renderDbStatus(state) {
    var el = document.getElementById('db-status');
    var ok = state.databases && state.databases.agent_ledger && state.databases.broodlink_hot;

    if (ok) {
      el.innerHTML = '<div class="alert alert-success"><strong>Databases ready.</strong> agent_ledger and broodlink_hot are configured.</div>';
      document.getElementById('init-db-btn').disabled = true;
      document.getElementById('init-db-btn').textContent = 'Already Created';
      document.getElementById('next-2-btn').disabled = false;
    } else {
      el.innerHTML = '<p>Databases need to be created. This sets up the agent memory ledger and task queue.</p>';
      document.getElementById('next-2-btn').disabled = true;
    }
  }

  function initDatabases() {
    var btn = document.getElementById('init-db-btn');
    var el = document.getElementById('db-status');
    btn.disabled = true;
    btn.textContent = 'Creating...';
    el.innerHTML = '<p>Creating databases and running migrations... This may take a moment.</p>';

    fetch('/setup/init-databases', { method: 'POST' })
      .then(function (r) { return r.json(); })
      .then(function (data) {
        if (data.success) {
          el.innerHTML = '<div class="alert alert-success"><strong>Done!</strong> ' + BL.escapeHtml(data.message) + '</div>';
          document.getElementById('next-2-btn').disabled = false;
          btn.textContent = 'Created';
        } else {
          el.innerHTML = '<div class="alert alert-error"><strong>Failed:</strong> ' + BL.escapeHtml(data.message) + '</div>';
          btn.textContent = 'Retry';
          btn.disabled = false;
        }
      })
      .catch(function (err) {
        el.innerHTML = '<div class="alert alert-error">Error: ' + BL.escapeHtml(err.message || 'unknown') + '</div>';
        btn.textContent = 'Retry';
        btn.disabled = false;
      });
  }

  // -- Step 3: Models --

  function loadRecommendedModels() {
    fetch('/api/v1/models/recommended')
      .then(function (r) { return r.json(); })
      .then(function (models) {
        renderModelCheckboxes(models);
      })
      .catch(function () {
        document.getElementById('models-list').innerHTML = '<p>Failed to load model list.</p>';
      });
  }

  function renderModelCheckboxes(models) {
    var el = document.getElementById('models-list');
    var installedNames = (systemState && systemState.models)
      ? systemState.models.map(function (m) { return m.name; })
      : [];

    var html = models.map(function (m) {
      var installed = installedNames.indexOf(m.name) !== -1;
      var checked = m.required || installed;
      var disabledAttr = installed ? ' disabled' : '';

      return '<div class="model-row">' +
        '<input type="checkbox" id="model-' + m.name + '" value="' + BL.escapeHtml(m.name) + '"' +
        (checked ? ' checked' : '') + disabledAttr + '>' +
        '<label for="model-' + m.name + '">' +
          '<strong>' + BL.escapeHtml(m.name) + '</strong><br>' +
          '<span style="color:var(--text-secondary);font-size:0.85rem;">' + BL.escapeHtml(m.description) + '</span>' +
        '</label>' +
        '<span class="model-role' + (m.required ? ' required' : '') + '">' + (m.required ? 'required' : m.role) + '</span>' +
        '<span class="model-size">' + m.size + '</span>' +
        (installed ? '<span style="color:var(--green);font-size:0.85rem;">Installed</span>' : '') +
        '</div>';
    }).join('');

    el.innerHTML = html;
  }

  function pullSelectedModels() {
    var checkboxes = document.querySelectorAll('#models-list input[type="checkbox"]:checked:not(:disabled)');
    var names = [];
    for (var i = 0; i < checkboxes.length; i++) {
      names.push(checkboxes[i].value);
    }

    if (names.length === 0) {
      document.getElementById('next-3-btn').disabled = false;
      return;
    }

    var btn = document.getElementById('pull-models-btn');
    var progress = document.getElementById('pull-progress');
    btn.disabled = true;
    btn.textContent = 'Downloading...';
    progress.style.display = 'block';

    var total = names.length;
    var done = 0;

    function pullNext() {
      if (done >= total) {
        btn.textContent = 'Download Selected';
        btn.disabled = false;
        progress.innerHTML = '<div class="alert alert-success"><strong>All models downloaded!</strong></div>';
        document.getElementById('next-3-btn').disabled = false;
        refreshStatus();
        return;
      }

      var name = names[done];
      progress.innerHTML = '<p>Downloading <strong>' + BL.escapeHtml(name) + '</strong> (' + (done + 1) + '/' + total + ')...</p>' +
        '<div class="progress-bar"><div class="progress-bar-fill" style="width:' + Math.round(((done + 0.5) / total) * 100) + '%"></div></div>';

      fetch('/api/v1/models/pull', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name: name })
      })
        .then(function (r) { return r.json(); })
        .then(function () {
          // Wait for the pull to actually complete by polling
          return waitForModel(name);
        })
        .then(function () {
          done++;
          pullNext();
        })
        .catch(function (err) {
          progress.innerHTML += '<div class="alert alert-error">Failed to pull ' + BL.escapeHtml(name) + ': ' + BL.escapeHtml(err.message || 'unknown') + '</div>';
          done++;
          pullNext();
        });
    }

    pullNext();
  }

  function waitForModel(name) {
    return new Promise(function (resolve) {
      var attempts = 0;
      var interval = setInterval(function () {
        attempts++;
        fetch('/api/v1/models')
          .then(function (r) { return r.json(); })
          .then(function (data) {
            var found = (data.models || []).some(function (m) { return m.name === name; });
            if (found || attempts > 360) { // 30 min max wait
              clearInterval(interval);
              resolve();
            }
          })
          .catch(function () {
            if (attempts > 360) {
              clearInterval(interval);
              resolve();
            }
          });
      }, 5000);
    });
  }

  // -- Step 4: Launch --

  function launchServices() {
    var btn = document.getElementById('launch-btn');
    var el = document.getElementById('launch-status');
    btn.disabled = true;
    btn.textContent = 'Starting...';
    el.innerHTML = '<p>Starting all Broodlink services...</p>';

    fetch('/setup/start-services', { method: 'POST' })
      .then(function (r) { return r.json(); })
      .then(function (data) {
        if (data.success) {
          el.innerHTML = '<p>Services started. Finalizing setup...</p>';
          // Tell the server setup is done — this unlocks the dashboard
          return fetch('/setup/complete', { method: 'POST' })
            .then(function (r) { return r.json(); })
            .then(function () {
              el.innerHTML = '<div class="alert alert-success">' +
                '<strong>Broodlink is running!</strong><br>' +
                'Redirecting to the dashboard...' +
                '</div>';
              setTimeout(function () { window.location.href = '/'; }, 1500);
            });
        } else {
          el.innerHTML = '<div class="alert alert-error"><strong>Some services failed:</strong> ' + BL.escapeHtml(data.message) + '</div>';
          btn.textContent = 'Retry';
          btn.disabled = false;
        }
      })
      .catch(function (err) {
        el.innerHTML = '<div class="alert alert-error">Error: ' + BL.escapeHtml(err.message || 'unknown') + '</div>';
        btn.textContent = 'Retry';
        btn.disabled = false;
      });
  }

  // -- Status refresh --

  function refreshStatus() {
    fetch('/setup/status')
      .then(function (r) { return r.json(); })
      .then(function (state) {
        systemState = state;
        renderDeps(state);
        renderDbStatus(state);
      })
      .catch(function () {
        document.getElementById('deps-tbody').innerHTML =
          '<tr><td colspan="4">Failed to check system status. Is the server running?</td></tr>';
      });
  }

  // -- Wire up buttons --

  document.getElementById('install-all-btn').addEventListener('click', installAllMissing);
  document.getElementById('init-db-btn').addEventListener('click', initDatabases);
  document.getElementById('pull-models-btn').addEventListener('click', pullSelectedModels);
  document.getElementById('launch-btn').addEventListener('click', launchServices);

  document.getElementById('next-1-btn').addEventListener('click', function () { goToStep(1); });
  document.getElementById('prev-2-btn').addEventListener('click', function () { goToStep(0); });
  document.getElementById('next-2-btn').addEventListener('click', function () { goToStep(2); loadRecommendedModels(); });
  document.getElementById('prev-3-btn').addEventListener('click', function () { goToStep(1); });
  document.getElementById('next-3-btn').addEventListener('click', function () { goToStep(3); });
  document.getElementById('skip-3-btn').addEventListener('click', function () { goToStep(3); });
  document.getElementById('prev-4-btn').addEventListener('click', function () { goToStep(2); });

  // -- Init --
  goToStep(0);
  refreshStatus();
})();
