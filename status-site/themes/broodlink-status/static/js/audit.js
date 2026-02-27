// Broodlink — Multi-agent AI orchestration
// Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
// SPDX-License-Identifier: AGPL-3.0-or-later

(function () {
  'use strict';

  var tbody = document.getElementById('audit-tbody');
  var agentFilter = document.getElementById('audit-agent-filter');
  var opFilter = document.getElementById('audit-operation-filter');
  if (!tbody) return;

  var BL = window.Broodlink;

  function updateMetrics(entries) {
    var total = entries.length;
    var ok = 0;
    var agents = {};
    entries.forEach(function (e) {
      if (e.result_status === 'ok') ok++;
      if (e.agent_id) agents[e.agent_id] = true;
    });
    var rate = total > 0 ? Math.round((ok / total) * 100) + '%' : '--';
    var el;
    el = document.getElementById('audit-total'); if (el) el.textContent = total;
    el = document.getElementById('audit-success-rate'); if (el) el.textContent = rate;
    el = document.getElementById('audit-agents'); if (el) el.textContent = Object.keys(agents).length;
  }

  function render(entries) {
    if (!entries || entries.length === 0) {
      tbody.innerHTML = '<tr><td colspan="5">No audit entries found.</td></tr>';
      return;
    }

    tbody.innerHTML = entries.map(function (e) {
      var id = BL.escapeHtml(String(e.id || '\u2014'));
      var agent = BL.escapeHtml(e.agent_id || '\u2014');
      var op = BL.escapeHtml(e.operation || '\u2014');
      var result = e.result_status || 'unknown';
      var resultClass = result === 'ok' ? 'ok' : (result === 'error' ? 'critical' : 'degraded');
      var resultLabel = result === 'ok' ? 'success' : BL.escapeHtml(result);
      var time = e.created_at ? BL.formatRelativeTime(e.created_at) : '\u2014';

      return '<tr>' +
        '<td><code>' + id + '</code></td>' +
        '<td>' + agent + '</td>' +
        '<td>' + op + '</td>' +
        '<td>' + BL.statusDot(resultClass) + ' ' + resultLabel + '</td>' +
        '<td>' + time + '</td>' +
      '</tr>';
    }).join('');
  }

  function buildQuery() {
    var params = [];
    if (agentFilter && agentFilter.value) params.push('agent_id=' + encodeURIComponent(agentFilter.value));
    if (opFilter && opFilter.value) params.push('operation=' + encodeURIComponent(opFilter.value));
    return params.length > 0 ? '?' + params.join('&') : '';
  }

  function load() {
    BL.fetchApi('/api/v1/audit' + buildQuery()).then(function (data) {
      var entries = data.audit || data.entries || data;
      updateMetrics(entries);
      render(entries);
    }).catch(function () {
      tbody.innerHTML = '<tr><td colspan="5">Failed to load audit log.</td></tr>';
    });
  }

  load();
  setInterval(load, BL.REFRESH_INTERVAL || 10000);

  if (agentFilter) agentFilter.addEventListener('input', BL.debounce(load, 400));
  if (opFilter) opFilter.addEventListener('input', BL.debounce(load, 400));
})();
