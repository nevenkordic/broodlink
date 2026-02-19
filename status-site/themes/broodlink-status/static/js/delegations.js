/*
 * Broodlink - Multi-agent AI orchestration system
 * Copyright (C) 2025-2026 Neven Kordic <neven@broodlink.ai>
 * SPDX-License-Identifier: AGPL-3.0-or-later
 */

(function () {
  'use strict';

  var container = document.getElementById('delegations-table');
  var filter = document.getElementById('delegation-status-filter');
  if (!container) return;

  var BL = window.Broodlink;

  function statusBadge(status) {
    var colors = {
      pending: '#f59e0b',
      accepted: '#22c55e',
      rejected: '#ef4444',
      in_progress: '#3b82f6',
      completed: '#22c55e',
      failed: '#ef4444'
    };
    var color = colors[status] || '#6b7280';
    return '<span style="display:inline-flex;align-items:center;gap:4px;">' +
      '<svg width="8" height="8"><circle cx="4" cy="4" r="4" fill="' + color + '"/></svg>' +
      BL.escapeHtml(status) + '</span>';
  }

  function renderTable(delegations) {
    if (!delegations || delegations.length === 0) {
      container.innerHTML = '<p>No delegations found.</p>';
      return;
    }

    var html = '<table><thead><tr>' +
      '<th>Title</th><th>From</th><th>To</th><th>Status</th>' +
      '<th data-tooltip="The originating task that spawned this delegation">Parent Task</th><th>Created</th><th>Updated</th>' +
      '</tr></thead><tbody>';

    delegations.forEach(function (d) {
      html += '<tr>' +
        '<td><strong>' + BL.escapeHtml(d.title) + '</strong>';
      if (d.description) {
        html += '<br><small>' + BL.escapeHtml(d.description) + '</small>';
      }
      html += '</td>' +
        '<td>' + BL.escapeHtml(d.from_agent) + '</td>' +
        '<td>' + BL.escapeHtml(d.to_agent) + '</td>' +
        '<td>' + statusBadge(d.status) + '</td>' +
        '<td>' + BL.escapeHtml(d.parent_task_id || '-') + '</td>' +
        '<td>' + BL.formatRelativeTime(d.created_at) + '</td>' +
        '<td>' + BL.formatRelativeTime(d.updated_at) + '</td>' +
        '</tr>';

      // Show result if completed
      if (d.result && d.status === 'completed') {
        html += '<tr><td colspan="7" style="padding-left:2rem;opacity:0.8;">' +
          '<code>' + BL.escapeHtml(JSON.stringify(d.result)) + '</code></td></tr>';
      }
    });

    html += '</tbody></table>';
    container.innerHTML = html;
  }

  function load() {
    BL.fetchApi('/api/v1/delegations').then(function (data) {
      var delegations = data.delegations || [];
      var statusVal = filter ? filter.value : '';
      if (statusVal) {
        delegations = delegations.filter(function (d) { return d.status === statusVal; });
      }
      renderTable(delegations);
    }).catch(function () {
      container.innerHTML = '<p>Failed to load delegations.</p>';
    });
  }

  if (filter) {
    filter.addEventListener('change', load);
  }

  load();
  setInterval(load, BL.REFRESH_INTERVAL || 10000);
})();
