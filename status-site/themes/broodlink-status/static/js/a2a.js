// Broodlink — Multi-agent AI orchestration
// Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
// SPDX-License-Identifier: AGPL-3.0-or-later

(function () {
  'use strict';

  var cardEl = document.getElementById('a2a-card');
  var tasksEl = document.getElementById('a2a-tasks');
  if (!cardEl && !tasksEl) return;

  var BL = window.Broodlink;

  function loadCard() {
    if (!cardEl) return;
    BL.fetchApi('/api/v1/a2a/card').then(function (data) {
      cardEl.textContent = JSON.stringify(data.card || data, null, 2);
    }).catch(function () {
      cardEl.textContent = 'Failed to load AgentCard.';
    });
  }

  function renderTasks(tasks) {
    if (!tasksEl) return;
    if (!tasks || tasks.length === 0) {
      tasksEl.innerHTML = '<p>No A2A task mappings.</p>';
      return;
    }

    var html = '<table class="data-table" role="table">' +
      '<thead><tr>' +
        '<th>External ID</th>' +
        '<th>Internal ID</th>' +
        '<th>Source</th>' +
        '<th>Created</th>' +
      '</tr></thead><tbody>';

    tasks.forEach(function (t) {
      html += '<tr>' +
        '<td><code>' + BL.escapeHtml(t.external_id || '') + '</code></td>' +
        '<td><code>' + BL.escapeHtml(t.internal_id || '') + '</code></td>' +
        '<td>' + BL.escapeHtml(t.source_agent || '—') + '</td>' +
        '<td>' + (t.created_at ? BL.formatRelativeTime(t.created_at) : '—') + '</td>' +
      '</tr>';
    });

    html += '</tbody></table>';
    tasksEl.innerHTML = html;
  }

  function loadTasks() {
    BL.fetchApi('/api/v1/a2a/tasks').then(function (data) {
      renderTasks(data.tasks || []);
    }).catch(function () {
      if (tasksEl) tasksEl.innerHTML = '<p>Failed to load A2A tasks.</p>';
    });
  }

  loadCard();
  loadTasks();
  setInterval(loadTasks, BL.REFRESH_INTERVAL || 10000);
})();
