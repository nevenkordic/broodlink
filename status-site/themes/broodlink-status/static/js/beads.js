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

  function updateMetrics(issues) {
    var total = issues.length;
    var open = 0, inProgress = 0, closed = 0;
    issues.forEach(function (i) {
      if (i.status === 'open') open++;
      else if (i.status === 'in_progress') inProgress++;
      else if (i.status === 'closed') closed++;
    });
    var el;
    el = document.getElementById('beads-total'); if (el) el.textContent = total;
    el = document.getElementById('beads-open'); if (el) el.textContent = open;
    el = document.getElementById('beads-in-progress'); if (el) el.textContent = inProgress;
    el = document.getElementById('beads-closed'); if (el) el.textContent = closed;
  }

  function render(issues) {
    if (!issues || issues.length === 0) {
      tbody.innerHTML = '<tr><td colspan="6">No issues found.</td></tr>';
      return;
    }

    tbody.innerHTML = issues.map(function (i) {
      var beadId = BL.escapeHtml(i.bead_id || '\u2014');
      var title = BL.escapeHtml(i.title || '\u2014');
      var assignee = BL.escapeHtml(i.assignee || '\u2014');
      var convoy = BL.escapeHtml(i.convoy_id || '\u2014');
      var time = i.updated_at ? BL.formatRelativeTime(i.updated_at) : '\u2014';

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

  var allIssues = [];

  function load() {
    BL.fetchApi('/api/v1/beads').then(function (data) {
      allIssues = data.issues || data;
      updateMetrics(allIssues);

      var filtered = allIssues;
      var statusVal = statusFilter && statusFilter.value ? statusFilter.value : '';
      if (statusVal) {
        filtered = allIssues.filter(function (i) { return i.status === statusVal; });
      }
      render(filtered);
    }).catch(function () {
      tbody.innerHTML = '<tr><td colspan="6">Failed to load issues.</td></tr>';
    });
  }

  load();
  setInterval(load, BL.REFRESH_INTERVAL || 10000);

  if (statusFilter) statusFilter.addEventListener('change', load);
})();
