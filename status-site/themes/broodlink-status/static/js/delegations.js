/*
 * Broodlink - Multi-agent AI orchestration system
 * Copyright (C) 2025-2026 Neven Kordic <neven@broodlink.ai>
 * SPDX-License-Identifier: AGPL-3.0-or-later
 */

(function () {
  'use strict';

  var tbody = document.getElementById('delegations-tbody');
  var filter = document.getElementById('delegation-status-filter');
  if (!tbody) return;

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

  function updateMetrics(delegations) {
    var total = delegations.length;
    var pending = 0, completed = 0, failed = 0;
    delegations.forEach(function (d) {
      if (d.status === 'pending') pending++;
      else if (d.status === 'completed') completed++;
      else if (d.status === 'failed') failed++;
    });
    var el;
    el = document.getElementById('deleg-total'); if (el) el.textContent = total;
    el = document.getElementById('deleg-pending'); if (el) el.textContent = pending;
    el = document.getElementById('deleg-completed'); if (el) el.textContent = completed;
    el = document.getElementById('deleg-failed'); if (el) el.textContent = failed;
  }

  function renderRows(delegations) {
    if (!delegations || delegations.length === 0) {
      tbody.innerHTML = '<tr><td colspan="7">No delegations found.</td></tr>';
      return;
    }

    var html = '';
    delegations.forEach(function (d) {
      html += '<tr>' +
        '<td><strong>' + BL.escapeHtml(d.title) + '</strong>' +
        (d.description ? '<br><small>' + BL.escapeHtml(d.description) + '</small>' : '') +
        '</td>' +
        '<td>' + BL.escapeHtml(d.from_agent) + '</td>' +
        '<td>' + BL.escapeHtml(d.to_agent) + '</td>' +
        '<td>' + statusBadge(d.status) + '</td>' +
        '<td>' + BL.escapeHtml(d.parent_task_id || '-') + '</td>' +
        '<td>' + BL.formatRelativeTime(d.created_at) + '</td>' +
        '<td>' + BL.formatRelativeTime(d.updated_at) + '</td>' +
        '</tr>';

      if (d.result && d.status === 'completed') {
        html += '<tr><td colspan="7" style="padding-left:2rem;opacity:0.8;">' +
          '<code>' + BL.escapeHtml(JSON.stringify(d.result)) + '</code></td></tr>';
      }
    });

    tbody.innerHTML = html;
  }

  var allDelegations = [];

  function load() {
    BL.fetchApi('/api/v1/delegations').then(function (data) {
      allDelegations = data.delegations || [];
      updateMetrics(allDelegations);

      var filtered = allDelegations;
      var statusVal = filter ? filter.value : '';
      if (statusVal) {
        filtered = allDelegations.filter(function (d) { return d.status === statusVal; });
      }
      renderRows(filtered);
    }).catch(function () {
      tbody.innerHTML = '<tr><td colspan="7">Failed to load delegations.</td></tr>';
    });
  }

  if (filter) {
    filter.addEventListener('change', load);
  }

  load();
  setInterval(load, BL.REFRESH_INTERVAL || 10000);

  // ── Negotiations section ──────────────────────────────────────────
  var negTbody = document.getElementById('negotiations-tbody');
  var negFilter = document.getElementById('neg-action-filter');
  if (!negTbody) return;

  var allNegotiations = [];

  function actionBadge(action) {
    var colors = {
      declined: '#ef4444',
      context_requested: '#f59e0b',
      context_provided: '#3b82f6',
      redirected: '#8b5cf6'
    };
    var color = colors[action] || '#6b7280';
    var label = action.replace(/_/g, ' ');
    return '<span style="display:inline-flex;align-items:center;gap:4px;">' +
      '<svg width="8" height="8"><circle cx="4" cy="4" r="4" fill="' + color + '"/></svg>' +
      BL.escapeHtml(label) + '</span>';
  }

  function updateNegMetrics(negs) {
    var total = negs.length, declined = 0, context = 0, redirected = 0;
    negs.forEach(function (n) {
      if (n.action === 'declined') declined++;
      else if (n.action === 'context_requested' || n.action === 'context_provided') context++;
      else if (n.action === 'redirected') redirected++;
    });
    var el;
    el = document.getElementById('neg-total'); if (el) el.textContent = total;
    el = document.getElementById('neg-declined'); if (el) el.textContent = declined;
    el = document.getElementById('neg-context'); if (el) el.textContent = context;
    el = document.getElementById('neg-redirected'); if (el) el.textContent = redirected;
  }

  function renderNegotiations(negs) {
    if (!negs || negs.length === 0) {
      negTbody.innerHTML = '<tr><td colspan="6">No negotiation events found.</td></tr>';
      return;
    }
    negTbody.innerHTML = negs.map(function (n) {
      return '<tr>' +
        '<td class="mono">' + BL.escapeHtml(n.task_id) + '</td>' +
        '<td>' + BL.escapeHtml(n.agent_id) + '</td>' +
        '<td>' + actionBadge(n.action) + '</td>' +
        '<td>' + BL.escapeHtml(n.reason || '-') + '</td>' +
        '<td>' + BL.escapeHtml(n.suggested_agent || '-') + '</td>' +
        '<td>' + BL.formatRelativeTime(n.created_at) + '</td>' +
        '</tr>';
    }).join('');
  }

  function loadNegotiations() {
    BL.fetchApi('/api/v1/negotiations').then(function (data) {
      allNegotiations = data.negotiations || [];
      updateNegMetrics(allNegotiations);
      var filtered = allNegotiations;
      var actionVal = negFilter ? negFilter.value : '';
      if (actionVal) {
        filtered = allNegotiations.filter(function (n) { return n.action === actionVal; });
      }
      renderNegotiations(filtered);
    }).catch(function () {
      negTbody.innerHTML = '<tr><td colspan="6">Failed to load negotiations.</td></tr>';
    });
  }

  if (negFilter) negFilter.addEventListener('change', loadNegotiations);
  loadNegotiations();
  setInterval(loadNegotiations, BL.REFRESH_INTERVAL || 10000);
})();
