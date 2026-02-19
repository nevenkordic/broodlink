// Broodlink — Multi-agent AI orchestration
// Copyright (C) 2025-2026 Neven Kordic <neven@broodlink.ai>
// SPDX-License-Identifier: AGPL-3.0-or-later

(function () {
  'use strict';

  var tbody = document.getElementById('beads-tbody');
  var statusFilter = document.getElementById('beads-status-filter');
  if (!tbody) return;

  var BL = window.Broodlink;

  function statusBadge(status) {
    var s = status || 'open';
    var cls = s === 'closed' ? 'ok' : (s === 'in_progress' ? 'degraded' : 'critical');
    return BL.statusDot(cls) + ' ' + BL.escapeHtml(s);
  }

  function render(issues) {
    if (!issues || issues.length === 0) {
      tbody.innerHTML = '<tr><td colspan="6">No issues found.</td></tr>';
      return;
    }

    tbody.innerHTML = issues.map(function (i) {
      var beadId = BL.escapeHtml(i.bead_id || '—');
      var title = BL.escapeHtml(i.title || '—');
      var assignee = BL.escapeHtml(i.assignee || '—');
      var convoy = BL.escapeHtml(i.convoy_id || '—');
      var time = i.updated_at ? BL.formatRelativeTime(i.updated_at) : '—';

      return '<tr>' +
        '<td><code>' + beadId + '</code></td>' +
        '<td>' + title + '</td>' +
        '<td>' + statusBadge(i.status) + '</td>' +
        '<td>' + assignee + '</td>' +
        '<td>' + convoy + '</td>' +
        '<td>' + time + '</td>' +
      '</tr>';
    }).join('');
  }

  function load() {
    var query = statusFilter && statusFilter.value ? '?status=' + encodeURIComponent(statusFilter.value) : '';
    BL.fetchApi('/api/v1/beads' + query).then(function (data) {
      render(data.issues || data);
    }).catch(function () {
      tbody.innerHTML = '<tr><td colspan="6">Failed to load issues.</td></tr>';
    });
  }

  load();
  setInterval(load, BL.REFRESH_INTERVAL || 10000);

  if (statusFilter) statusFilter.addEventListener('change', load);
})();
