/* Agent Onboarding — v0.11.0 */
(function () {
  'use strict';
  var BL = window.Broodlink;
  var esc = BL.escapeHtml;

  // POST helper (same pattern as control.js)
  function postApi(path, body) {
    var hdrs = {
      'Content-Type': 'application/json',
      'Accept': 'application/json',
      'X-Broodlink-Api-Key': BL.STATUS_API_KEY
    };
    var sessionToken = typeof sessionStorage !== 'undefined'
      ? sessionStorage.getItem('broodlink_session_token')
      : null;
    if (sessionToken) hdrs['X-Broodlink-Session'] = sessionToken;

    return fetch(BL.STATUS_API_URL + path, {
      method: 'POST',
      headers: hdrs,
      body: JSON.stringify(body)
    }).then(function (res) {
      if (!res.ok) return res.json().then(function (e) { throw new Error(e.error || 'request failed'); });
      return res.json();
    }).then(function (json) { return json.data !== undefined ? json.data : json; });
  }

  function badge(text, type) {
    return '<span class="ctrl-badge ctrl-badge-' + type + '">' + esc(text) + '</span>';
  }

  function loadAgents() {
    var tbody = document.getElementById('agents-table-body');
    tbody.innerHTML = '<tr><td colspan="7" class="loading-cell">Loading agents...</td></tr>';

    BL.fetchApi('/api/v1/agents/tokens').then(function (data) {
      renderTable(data.agents || []);
    }).catch(function () {
      tbody.innerHTML =
        '<tr><td colspan="7" class="loading-cell">Failed to load agent data.</td></tr>';
    });
  }

  function renderTable(agents) {
    var tbody = document.getElementById('agents-table-body');
    if (!tbody) return;
    if (!agents.length) {
      tbody.innerHTML = '<tr><td colspan="7" class="loading-cell">No agents registered yet. Use the form above to onboard your first agent.</td></tr>';
      return;
    }
    tbody.innerHTML = agents.map(function (a) {
      var activeType = a.active ? 'ok' : 'offline';
      var jwtType = a.has_jwt ? 'ok' : 'failed';
      var created = a.created_at ? BL.formatRelativeTime(a.created_at) : '--';

      return '<tr>' +
        '<td><code>' + esc(a.agent_id) + '</code></td>' +
        '<td>' + esc(a.display_name || '--') + '</td>' +
        '<td>' + esc(a.role || '--') + '</td>' +
        '<td>' + esc(a.cost_tier || '--') + '</td>' +
        '<td>' + badge(a.active ? 'active' : 'inactive', activeType) + '</td>' +
        '<td>' + badge(a.has_jwt ? 'present' : 'missing', jwtType) + '</td>' +
        '<td>' + esc(created) + '</td>' +
        '</tr>';
    }).join('');
  }

  function copyToClipboard(text) {
    if (navigator.clipboard && navigator.clipboard.writeText) {
      return navigator.clipboard.writeText(text);
    }
    // Fallback for older browsers
    var ta = document.createElement('textarea');
    ta.value = text;
    ta.style.position = 'fixed';
    ta.style.left = '-9999px';
    document.body.appendChild(ta);
    ta.select();
    document.execCommand('copy');
    document.body.removeChild(ta);
    return Promise.resolve();
  }

  document.getElementById('onboard-form').addEventListener('submit', function (e) {
    e.preventDefault();
    var btn = document.getElementById('onboard-btn');
    var result = document.getElementById('onboard-result');
    btn.disabled = true;
    btn.textContent = 'Onboarding\u2026';

    var agentId = document.getElementById('agent-id').value.trim();
    var displayName = document.getElementById('display-name').value.trim() || agentId;
    var role = document.getElementById('agent-role').value;
    var costTier = document.getElementById('cost-tier').value;

    postApi('/api/v1/agents/onboard', {
      agent_id: agentId,
      display_name: displayName,
      role: role,
      cost_tier: costTier
    }).then(function (data) {
      var tokenId = 'jwt-token-' + Date.now();
      result.style.display = 'block';
      result.className = 'onboard-result';
      result.innerHTML =
        '<div class="alert alert-success">' +
        '<strong>Agent onboarded successfully!</strong>' +
        '<dl class="onboard-meta">' +
        '<dt>Agent ID</dt><dd><code>' + esc(data.agent_id) + '</code></dd>' +
        '<dt>Expires</dt><dd>' + esc(data.expires_at) + '</dd>' +
        '<dt>Token saved to</dt><dd><code>' + esc(data.token_path) + '</code></dd>' +
        '</dl>' +
        '<label for="' + tokenId + '">JWT Token</label>' +
        '<div class="jwt-wrap">' +
        '<textarea id="' + tokenId + '" class="jwt-output" rows="4" readonly>' +
        esc(data.jwt_token) +
        '</textarea>' +
        '<button type="button" class="btn btn-copy" data-token-id="' + tokenId + '">' +
        'Copy' +
        '</button>' +
        '</div>' +
        '</div>';
      btn.disabled = false;
      btn.textContent = 'Onboard Agent';
      document.getElementById('onboard-form').reset();
      loadAgents();
    }).catch(function (err) {
      result.style.display = 'block';
      result.className = 'onboard-result';
      result.innerHTML =
        '<div class="alert alert-error">Failed to onboard agent: ' +
        esc(err.message || String(err)) + '</div>';
      btn.disabled = false;
      btn.textContent = 'Onboard Agent';
    });
  });

  // Delegate copy button clicks
  document.addEventListener('click', function (e) {
    if (!e.target.classList.contains('btn-copy')) return;
    var ta = document.getElementById(e.target.dataset.tokenId);
    if (!ta) return;
    copyToClipboard(ta.value).then(function () {
      e.target.textContent = 'Copied!';
      setTimeout(function () { e.target.textContent = 'Copy'; }, 2000);
    });
  });

  loadAgents();
})();
