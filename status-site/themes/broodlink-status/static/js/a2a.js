// Broodlink — Multi-agent AI orchestration
// Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
// SPDX-License-Identifier: AGPL-3.0-or-later

(function () {
  'use strict';

  var cardEl = document.getElementById('a2a-card');
  var tasksTbody = document.getElementById('a2a-tasks-tbody');
  if (!cardEl && !tasksTbody) return;

  var BL = window.Broodlink;

  function loadCard() {
    if (!cardEl) return;
    BL.fetchApi('/api/v1/a2a/card').then(function (data) {
      var card = data.card || data;
      var name = card.name || card.agent_id || 'Unknown';
      var desc = card.description || '';
      var url = card.url || '';
      var version = card.version || '';
      var skills = card.skills || [];

      var html = '<dl class="a2a-info-grid">' +
        '<div><dt>Name</dt><dd>' + BL.escapeHtml(name) + '</dd></div>' +
        '<div><dt>URL</dt><dd>' + (url ? '<a href="' + BL.escapeHtml(url) + '" target="_blank">' + BL.escapeHtml(url) + '</a>' : '\u2014') + '</dd></div>' +
        (version ? '<div><dt>Version</dt><dd>' + BL.escapeHtml(version) + '</dd></div>' : '') +
        (desc ? '<div><dt>Description</dt><dd>' + BL.escapeHtml(desc) + '</dd></div>' : '') +
        '</dl>';

      if (skills.length > 0) {
        html += '<h4 style="font-size:0.75rem;text-transform:uppercase;letter-spacing:0.05em;color:var(--text-secondary);margin-bottom:0.375rem;">Skills</h4>';
        html += '<div class="a2a-skills-list">';
        skills.forEach(function (s) {
          var label = typeof s === 'string' ? s : (s.name || s.id || JSON.stringify(s));
          html += '<span class="a2a-skill-tag">' + BL.escapeHtml(label) + '</span>';
        });
        html += '</div>';
      }

      cardEl.innerHTML = html;

      // Update skills metric
      var el = document.getElementById('a2a-skills');
      if (el) el.textContent = skills.length;
    }).catch(function () {
      cardEl.innerHTML = '<p class="empty-state">Failed to load agent card.</p>';
    });
  }

  function renderTasks(tasks) {
    if (!tasksTbody) return;

    // Update metric
    var el = document.getElementById('a2a-tasks-count');
    if (el) el.textContent = tasks.length;

    if (!tasks || tasks.length === 0) {
      tasksTbody.innerHTML = '<tr><td colspan="4">No A2A task mappings.</td></tr>';
      return;
    }

    tasksTbody.innerHTML = tasks.map(function (t) {
      return '<tr>' +
        '<td><code>' + BL.escapeHtml(t.external_id || '') + '</code></td>' +
        '<td><code>' + BL.escapeHtml(t.internal_id || '') + '</code></td>' +
        '<td>' + BL.escapeHtml(t.source_agent || '\u2014') + '</td>' +
        '<td>' + (t.created_at ? BL.formatRelativeTime(t.created_at) : '\u2014') + '</td>' +
      '</tr>';
    }).join('');
  }

  function loadTasks() {
    BL.fetchApi('/api/v1/a2a/tasks').then(function (data) {
      renderTasks(data.tasks || []);
    }).catch(function () {
      if (tasksTbody) tasksTbody.innerHTML = '<tr><td colspan="4">Failed to load A2A tasks.</td></tr>';
    });
  }

  loadCard();
  loadTasks();
  setInterval(loadTasks, BL.REFRESH_INTERVAL || 10000);
})();
