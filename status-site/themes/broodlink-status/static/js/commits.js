// Broodlink — Multi-agent AI orchestration
// Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
// SPDX-License-Identifier: AGPL-3.0-or-later

(function () {
  'use strict';

  var tbody = document.getElementById('commits-tbody');
  if (!tbody) return;

  var BL = window.Broodlink;

  function updateMetrics(commits) {
    var el;
    el = document.getElementById('commits-total'); if (el) el.textContent = commits.length;
    el = document.getElementById('commits-latest');
    if (el && commits.length > 0) {
      var latest = commits[0];
      var date = latest.date || latest.committed_at || latest.created_at || '';
      el.textContent = date ? BL.formatRelativeTime(date) : '--';
    }
  }

  function render(commits) {
    if (!commits || commits.length === 0) {
      tbody.innerHTML = '<tr><td colspan="4">No commits found.</td></tr>';
      return;
    }

    tbody.innerHTML = commits.map(function (c) {
      var hash = BL.escapeHtml((c.commit_hash || c.hash || '').substring(0, 8));
      var author = BL.escapeHtml(c.committer || c.author || '\u2014');
      var message = BL.escapeHtml(c.message || c.commit_message || '\u2014');
      var date = c.date || c.committed_at || c.created_at || '';
      var formatted = date ? BL.formatRelativeTime(date) : '\u2014';

      return '<tr>' +
        '<td><code>' + hash + '</code></td>' +
        '<td>' + author + '</td>' +
        '<td>' + message + '</td>' +
        '<td>' + formatted + '</td>' +
      '</tr>';
    }).join('');
  }

  function load() {
    BL.fetchApi('/api/v1/commits').then(function (data) {
      var commits = data.commits || data;
      updateMetrics(commits);
      render(commits);
    }).catch(function () {
      tbody.innerHTML = '<tr><td colspan="4">Failed to load commits.</td></tr>';
    });
  }

  load();
  setInterval(load, BL.REFRESH_INTERVAL || 10000);
})();
