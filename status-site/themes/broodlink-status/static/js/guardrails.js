/*
 * Broodlink - Multi-agent AI orchestration system
 * Copyright (C) 2025-2026 Neven Kordic <neven@broodlink.ai>
 * SPDX-License-Identifier: AGPL-3.0-or-later
 */

(function () {
  'use strict';

  var policiesEl = document.getElementById('guardrail-policies');
  var violationsEl = document.getElementById('violations-table');
  var agentFilter = document.getElementById('violation-agent-filter');
  if (!policiesEl) return;

  var BL = window.Broodlink;

  var ruleTypeColors = {
    tool_block: '#ef4444',
    rate_override: '#f59e0b',
    content_filter: '#3b82f6',
    scope_limit: '#8b5cf6'
  };

  function renderPolicies(policies) {
    if (!policies || policies.length === 0) {
      policiesEl.innerHTML = '<p>No guardrail policies configured.</p>';
      return;
    }

    var html = '';
    policies.forEach(function (p) {
      var color = ruleTypeColors[p.rule_type] || '#6b7280';
      var statusLabel = p.enabled ? 'enabled' : 'disabled';
      var statusColor = p.enabled ? '#22c55e' : '#6b7280';

      html += '<div class="card">' +
        '<div class="card-header">' +
        '<span style="display:inline-flex;align-items:center;gap:6px;">' +
        '<svg width="10" height="10"><circle cx="5" cy="5" r="5" fill="' + color + '"/></svg>' +
        '<strong>' + BL.escapeHtml(p.name) + '</strong>' +
        '</span>' +
        '<span style="display:inline-flex;align-items:center;gap:4px;font-size:0.85em;">' +
        '<svg width="8" height="8"><circle cx="4" cy="4" r="4" fill="' + statusColor + '"/></svg>' +
        statusLabel + '</span>' +
        '</div>' +
        '<div class="card-body">' +
        '<div style="margin-bottom:0.5rem;">' +
        '<span class="badge" data-tooltip="tool_block, rate_override, content_filter, or scope_limit">' + BL.escapeHtml(p.rule_type) + '</span>' +
        '</div>' +
        '<code style="font-size:0.8em;word-break:break-all;">' +
        BL.escapeHtml(JSON.stringify(p.config)) + '</code>' +
        '</div>' +
        '<div class="card-footer">' +
        'Updated ' + BL.formatRelativeTime(p.updated_at) +
        '</div></div>';
    });

    policiesEl.innerHTML = html;
  }

  function renderViolations(violations) {
    if (!violations || violations.length === 0) {
      violationsEl.innerHTML = '<p>No violations recorded.</p>';
      return;
    }

    // Populate agent filter dropdown
    var agents = [];
    violations.forEach(function (v) {
      if (agents.indexOf(v.agent_id) === -1) agents.push(v.agent_id);
    });
    if (agentFilter && agentFilter.options.length <= 1) {
      agents.forEach(function (a) {
        var opt = document.createElement('option');
        opt.value = a;
        opt.textContent = a;
        agentFilter.appendChild(opt);
      });
    }

    // Apply filter
    var filterVal = agentFilter ? agentFilter.value : '';
    if (filterVal) {
      violations = violations.filter(function (v) { return v.agent_id === filterVal; });
    }

    var html = '<table><thead><tr>' +
      '<th>Agent</th><th>Policy</th><th>Tool</th><th>Details</th><th>When</th>' +
      '</tr></thead><tbody>';

    violations.forEach(function (v) {
      html += '<tr>' +
        '<td>' + BL.escapeHtml(v.agent_id) + '</td>' +
        '<td>' + BL.escapeHtml(v.policy_name) + '</td>' +
        '<td>' + BL.escapeHtml(v.tool_name || '-') + '</td>' +
        '<td>' + BL.escapeHtml(v.details || '-') + '</td>' +
        '<td>' + BL.formatRelativeTime(v.created_at) + '</td>' +
        '</tr>';
    });

    html += '</tbody></table>';
    violationsEl.innerHTML = html;
  }

  function load() {
    BL.fetchApi('/api/v1/guardrails').then(function (data) {
      renderPolicies(data.policies || []);
    }).catch(function () {
      policiesEl.innerHTML = '<p>Failed to load policies.</p>';
    });

    BL.fetchApi('/api/v1/violations').then(function (data) {
      renderViolations(data.violations || []);
    }).catch(function () {
      if (violationsEl) violationsEl.innerHTML = '<p>Failed to load violations.</p>';
    });
  }

  if (agentFilter) {
    agentFilter.addEventListener('change', load);
  }

  load();
  setInterval(load, BL.REFRESH_INTERVAL || 10000);
})();
