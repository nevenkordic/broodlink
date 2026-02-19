// Broodlink — Multi-agent AI orchestration
// Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
// SPDX-License-Identifier: AGPL-3.0-or-later

(function () {
  'use strict';

  var roster = document.getElementById('agent-roster');
  if (!roster) return;

  var BL = window.Broodlink;

  function renderMetrics(metrics) {
    if (!metrics) return '';
    var successRate = parseFloat(metrics.success_rate || 0);
    var rateClass = successRate >= 0.9 ? 'success-rate-good' :
                    successRate >= 0.7 ? 'success-rate-warn' : 'success-rate-bad';
    var ratePct = (successRate * 100).toFixed(0);
    var load = parseInt(metrics.current_load || 0, 10);
    var avgMs = parseInt(metrics.avg_response_ms || 0, 10);
    var score = parseFloat(metrics.routing_score || 0).toFixed(2);

    return '<div class="agent-metrics">' +
      '<span class="agent-score-badge" data-tooltip="Weighted score used by the coordinator for task routing">' + score + '</span>' +
      '<span class="' + rateClass + '" data-tooltip="Percentage of tool calls that returned ok">' + ratePct + '% success</span>' +
      '<span data-tooltip="Tasks currently assigned to this agent">' + load + ' active</span>' +
      (avgMs > 0 ? '<span data-tooltip="Average response time across recent tool calls">' + avgMs + 'ms avg</span>' : '') +
      '<div class="agent-metric-bar" title="Load level">' +
        '<div class="agent-metric-bar-fill" style="width:' + Math.min(load * 20, 100) + '%"></div>' +
      '</div>' +
    '</div>';
  }

  function render(agents, metricsMap) {
    if (!agents || agents.length === 0) {
      roster.innerHTML = '<p>No agents registered.</p>';
      return;
    }

    roster.innerHTML = agents.map(function (a) {
      var status = a.active ? 'ok' : 'offline';
      var lastSeen = a.updated_at ? BL.formatRelativeTime(a.updated_at) : 'never';
      var latestWork = '';
      if (a.latest_work) {
        var desc = (a.latest_work.action || '') + (a.latest_work.details ? ' — ' + a.latest_work.details : '');
        latestWork = '<div class="agent-latest">' + BL.escapeHtml(desc) + '</div>';
      }

      var metrics = metricsMap[a.agent_id] || null;

      return '<div class="card agent-card" role="article" aria-label="Agent ' + BL.escapeHtml(a.agent_id) + '">' +
        '<div class="agent-header">' +
          BL.statusDot(status) + ' ' +
          '<span class="agent-name">' + BL.escapeHtml(a.display_name || a.agent_id) + '</span>' +
          '<span style="font-size:0.75em;color:var(--text-secondary);margin-left:6px;">' +
            BL.escapeHtml(a.active ? 'active' : 'offline') + '</span>' +
        '</div>' +
        '<div class="agent-role">' + BL.escapeHtml(a.role || 'worker') + '</div>' +
        '<div class="agent-meta">Cost tier: ' + BL.escapeHtml(a.cost_tier || '—') +
          ' · Last seen: ' + lastSeen + '</div>' +
        renderMetrics(metrics) +
        latestWork +
      '</div>';
    }).join('');
  }

  function load() {
    // Fetch agents and metrics in parallel
    Promise.all([
      BL.fetchApi('/api/v1/agents'),
      BL.fetchApi('/api/v1/agent-metrics').catch(function () { return { metrics: [] }; })
    ]).then(function (results) {
      var agents = results[0].agents || results[0];
      var metricsList = results[1].metrics || results[1] || [];

      // Build map by agent_id for quick lookup
      var metricsMap = {};
      if (Array.isArray(metricsList)) {
        metricsList.forEach(function (m) {
          if (m.agent_id) metricsMap[m.agent_id] = m;
        });
      }

      render(agents, metricsMap);
    }).catch(function () {
      roster.innerHTML = '<p>Failed to load agents.</p>';
    });
  }

  load();
  setInterval(load, BL.REFRESH_INTERVAL || 10000);
})();
