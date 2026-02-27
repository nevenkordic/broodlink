/*
 * Broodlink - Multi-agent AI orchestration system
 * Copyright (C) 2025-2026 Neven Kordic <neven@broodlink.ai>
 * SPDX-License-Identifier: AGPL-3.0-or-later
 */

(function () {
  'use strict';

  var container = document.getElementById('approvals-table');
  var policiesContainer = document.getElementById('approval-policies');
  var filter = document.getElementById('approval-status-filter');
  var editor = document.getElementById('policy-editor');
  var editorTitle = document.getElementById('policy-editor-title');
  var form = document.getElementById('policy-form');
  var btnNew = document.getElementById('btn-new-policy');
  var btnCancel = document.getElementById('btn-cancel-policy');
  if (!container) return;

  var BL = window.Broodlink;

  function statusBadge(status) {
    var colors = {
      pending: '#f59e0b',
      approved: '#22c55e',
      auto_approved: '#86efac',
      rejected: '#ef4444',
      expired: '#6b7280'
    };
    var color = colors[status] || '#6b7280';
    return '<span style="display:inline-flex;align-items:center;gap:4px;">' +
      '<svg width="8" height="8"><circle cx="4" cy="4" r="4" fill="' + color + '"/></svg>' +
      BL.escapeHtml(status) + '</span>';
  }

  function severityBadge(severity) {
    if (!severity) return '-';
    var colors = { low: '#22c55e', medium: '#f59e0b', high: '#f97316', critical: '#ef4444' };
    var color = colors[severity] || '#6b7280';
    return '<span style="color:' + color + ';font-weight:600;">' +
      BL.escapeHtml(severity) + '</span>';
  }

  function formatPayload(payload) {
    if (!payload || typeof payload !== 'object') return '-';
    try {
      return '<pre style="margin:0;white-space:pre-wrap;font-size:0.85em;">' +
        BL.escapeHtml(JSON.stringify(payload, null, 2)) + '</pre>';
    } catch (e) {
      return BL.escapeHtml(String(payload));
    }
  }

  function renderTable(approvals) {
    if (!approvals || approvals.length === 0) {
      container.innerHTML = '<p>No approval gates found.</p>';
      return;
    }

    var html = '<table><thead><tr>' +
      '<th>Type</th><th>Severity</th><th>Requested By</th><th>Status</th>' +
      '<th>Payload</th><th>Task</th><th>Expires</th>' +
      '<th>Created</th><th>Reviewed</th><th>Actions</th>' +
      '</tr></thead><tbody>';

    approvals.forEach(function (a) {
      var severity = (a.payload && a.payload.severity) || null;
      html += '<tr>' +
        '<td>' + BL.escapeHtml(a.gate_type) + '</td>' +
        '<td>' + severityBadge(severity) + '</td>' +
        '<td>' + BL.escapeHtml(a.requested_by) + '</td>' +
        '<td>' + statusBadge(a.status) + '</td>' +
        '<td style="max-width:300px;overflow:auto;">' + formatPayload(a.payload) + '</td>' +
        '<td>' + BL.escapeHtml(a.task_id || '-') + '</td>' +
        '<td>' + (a.expires_at ? BL.formatRelativeTime(a.expires_at) : '-') + '</td>' +
        '<td>' + BL.formatRelativeTime(a.created_at) + '</td>' +
        '<td>';

      if (a.reviewed_by) {
        html += BL.escapeHtml(a.reviewed_by);
        if (a.reason) html += ': ' + BL.escapeHtml(a.reason);
        if (a.reviewed_at) html += '<br><small>' + BL.formatRelativeTime(a.reviewed_at) + '</small>';
      } else if (a.status === 'auto_approved') {
        html += '<em>auto-approved</em>';
        if (a.policy_id) html += '<br><small>policy: ' + BL.escapeHtml(a.policy_id.substring(0, 8)) + '</small>';
      } else {
        html += '-';
      }

      html += '</td><td>';

      if (a.status === 'pending') {
        html += '<button class="btn btn-approve" data-id="' + BL.escapeHtml(a.id) + '" ' +
          'data-type="' + BL.escapeHtml(a.gate_type) + '" ' +
          'data-agent="' + BL.escapeHtml(a.requested_by) + '" ' +
          'aria-label="Approve gate ' + BL.escapeHtml(a.id) + '">Approve</button> ' +
          '<button class="btn btn-reject" data-id="' + BL.escapeHtml(a.id) + '" ' +
          'data-type="' + BL.escapeHtml(a.gate_type) + '" ' +
          'data-agent="' + BL.escapeHtml(a.requested_by) + '" ' +
          'aria-label="Reject gate ' + BL.escapeHtml(a.id) + '">Reject</button>';
      } else {
        html += '-';
      }

      html += '</td></tr>';
    });

    html += '</tbody></table>';
    container.innerHTML = html;

    container.querySelectorAll('.btn-approve').forEach(function (btn) {
      btn.addEventListener('click', function () { reviewGate(btn.dataset.id, 'approved', btn); });
    });
    container.querySelectorAll('.btn-reject').forEach(function (btn) {
      btn.addEventListener('click', function () { reviewGate(btn.dataset.id, 'rejected', btn); });
    });
  }

  function renderPolicies(policies) {
    if (!policiesContainer) return;
    if (!policies || policies.length === 0) {
      policiesContainer.innerHTML = '<p>No approval policies configured. Click "+ New Policy" to create one.</p>';
      return;
    }

    var html = '<table><thead><tr>' +
      '<th>Name</th><th>Gate Type</th><th>Conditions</th>' +
      '<th>Auto-Approve</th><th>Threshold</th><th>Expiry</th><th>Active</th><th>Actions</th>' +
      '</tr></thead><tbody>';

    policies.forEach(function (p) {
      var condStr = '-';
      if (p.conditions && typeof p.conditions === 'object') {
        var parts = [];
        if (p.conditions.severity) parts.push('severity: ' + p.conditions.severity.join(', '));
        if (p.conditions.agent_ids) parts.push('agents: ' + p.conditions.agent_ids.join(', '));
        if (p.conditions.tool_names) parts.push('tools: ' + p.conditions.tool_names.join(', '));
        condStr = parts.length ? parts.join('; ') : 'any';
      }
      html += '<tr>' +
        '<td>' + BL.escapeHtml(p.name) + '</td>' +
        '<td>' + BL.escapeHtml(p.gate_type) + '</td>' +
        '<td>' + BL.escapeHtml(condStr) + '</td>' +
        '<td>' + (p.auto_approve ? 'Yes' : 'No') + '</td>' +
        '<td>' + (p.auto_approve ? p.auto_approve_threshold : '-') + '</td>' +
        '<td>' + p.expiry_minutes + ' min</td>' +
        '<td>' + statusBadge(p.active ? 'approved' : 'expired') + '</td>' +
        '<td>' +
          '<button class="btn btn-toggle" data-id="' + BL.escapeHtml(p.id) + '" ' +
            'data-name="' + BL.escapeHtml(p.name) + '">' +
            (p.active ? 'Disable' : 'Enable') + '</button> ' +
          '<button class="btn btn-edit" data-policy=\'' + BL.escapeHtml(JSON.stringify(p)) + '\'>' +
            'Edit</button>' +
        '</td></tr>';
    });

    html += '</tbody></table>';
    policiesContainer.innerHTML = html;

    policiesContainer.querySelectorAll('.btn-toggle').forEach(function (btn) {
      btn.addEventListener('click', function () { togglePolicy(btn.dataset.id, btn.dataset.name); });
    });
    policiesContainer.querySelectorAll('.btn-edit').forEach(function (btn) {
      btn.addEventListener('click', function () {
        try { editPolicy(JSON.parse(btn.dataset.policy)); } catch (e) { /* ignore */ }
      });
    });
  }

  // -----------------------------------------------------------------------
  // Policy editor
  // -----------------------------------------------------------------------

  function resetForm() {
    if (!form) return;
    form.reset();
    document.getElementById('pf-threshold').value = '0.8';
    document.getElementById('pf-expiry').value = '60';
    document.querySelectorAll('input[name="pf-severity"]').forEach(function (cb) { cb.checked = false; });
  }

  function showEditor(title) {
    if (editorTitle) editorTitle.textContent = title || 'New Approval Policy';
    if (editor) editor.style.display = 'block';
    editor.scrollIntoView({ behavior: 'smooth', block: 'nearest' });
  }

  function hideEditor() {
    if (editor) editor.style.display = 'none';
    resetForm();
  }

  function editPolicy(p) {
    if (!form) return;
    document.getElementById('pf-name').value = p.name || '';
    document.getElementById('pf-gate-type').value = p.gate_type || 'pre_dispatch';
    document.getElementById('pf-description').value = p.description || '';
    document.getElementById('pf-auto-approve').checked = !!p.auto_approve;
    document.getElementById('pf-threshold').value = p.auto_approve_threshold != null ? p.auto_approve_threshold : 0.8;
    document.getElementById('pf-expiry').value = p.expiry_minutes != null ? p.expiry_minutes : 60;

    // Severity checkboxes
    var sevs = (p.conditions && p.conditions.severity) || [];
    document.querySelectorAll('input[name="pf-severity"]').forEach(function (cb) {
      cb.checked = sevs.indexOf(cb.value) !== -1;
    });

    // Agent IDs
    var agents = (p.conditions && p.conditions.agent_ids) || [];
    document.getElementById('pf-agent-ids').value = agents.join(', ');

    // Tool names
    var tools = (p.conditions && p.conditions.tool_names) || [];
    document.getElementById('pf-tool-names').value = tools.join(', ');

    showEditor('Edit Policy: ' + p.name);
  }

  function getFormData() {
    var severity = [];
    document.querySelectorAll('input[name="pf-severity"]:checked').forEach(function (cb) {
      severity.push(cb.value);
    });

    var agentStr = document.getElementById('pf-agent-ids').value.trim();
    var agentIds = agentStr ? agentStr.split(',').map(function (s) { return s.trim(); }).filter(Boolean) : [];

    var toolStr = document.getElementById('pf-tool-names').value.trim();
    var toolNames = toolStr ? toolStr.split(',').map(function (s) { return s.trim(); }).filter(Boolean) : [];

    var conditions = {};
    if (severity.length) conditions.severity = severity;
    if (agentIds.length) conditions.agent_ids = agentIds;
    if (toolNames.length) conditions.tool_names = toolNames;

    return {
      name: document.getElementById('pf-name').value.trim(),
      gate_type: document.getElementById('pf-gate-type').value,
      description: document.getElementById('pf-description').value.trim() || undefined,
      conditions: conditions,
      auto_approve: document.getElementById('pf-auto-approve').checked,
      auto_approve_threshold: parseFloat(document.getElementById('pf-threshold').value) || 0.8,
      expiry_minutes: parseInt(document.getElementById('pf-expiry').value, 10) || 60,
      active: true
    };
  }

  function savePolicy(data) {
    var meta = document.querySelector('meta[name="broodlink:statusApiUrl"]');
    var apiUrl = (meta && meta.content) || 'http://localhost:3312';
    var keyMeta = document.querySelector('meta[name="broodlink:statusApiKey"]');
    var apiKey = (keyMeta && keyMeta.content) || '';

    fetch(apiUrl + '/api/v1/approval-policies', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', 'X-Broodlink-Api-Key': apiKey },
      body: JSON.stringify(data)
    })
    .then(function (resp) {
      if (!resp.ok) return resp.json().then(function (e) { throw new Error(e.error || 'save failed'); });
      return resp.json();
    })
    .then(function () {
      hideEditor();
      load();
    })
    .catch(function (err) { alert('Save failed: ' + err.message); });
  }

  function togglePolicy(policyId, policyName) {
    if (!confirm('Toggle active state for policy "' + policyName + '"?')) return;

    var meta = document.querySelector('meta[name="broodlink:statusApiUrl"]');
    var apiUrl = (meta && meta.content) || 'http://localhost:3312';
    var keyMeta = document.querySelector('meta[name="broodlink:statusApiKey"]');
    var apiKey = (keyMeta && keyMeta.content) || '';

    fetch(apiUrl + '/api/v1/approval-policies/' + policyId + '/toggle', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', 'X-Broodlink-Api-Key': apiKey }
    })
    .then(function (resp) {
      if (!resp.ok) return resp.json().then(function (e) { throw new Error(e.error || 'toggle failed'); });
      return resp.json();
    })
    .then(function () { load(); })
    .catch(function (err) { alert('Toggle failed: ' + err.message); });
  }

  // -----------------------------------------------------------------------
  // Wire up editor buttons
  // -----------------------------------------------------------------------

  if (btnNew) {
    btnNew.addEventListener('click', function () {
      resetForm();
      showEditor('New Approval Policy');
    });
  }

  if (btnCancel) {
    btnCancel.addEventListener('click', hideEditor);
  }

  if (form) {
    form.addEventListener('submit', function (e) {
      e.preventDefault();
      var data = getFormData();
      if (!data.name) { alert('Policy name is required.'); return; }
      savePolicy(data);
    });
  }

  // -----------------------------------------------------------------------
  // Gate review
  // -----------------------------------------------------------------------

  function reviewGate(gateId, decision, btn) {
    var verb = decision === 'approved' ? 'approve' : 'reject';
    var agent = btn ? btn.dataset.agent : '';
    var gateType = btn ? btn.dataset.type : '';
    var msg = 'Are you sure you want to ' + verb + ' this gate?\n\n' +
      'Gate: ' + gateId + '\n' +
      'Type: ' + gateType + '\n' +
      'Agent: ' + agent;
    if (!confirm(msg)) return;

    var reason = prompt('Reason (optional):');

    var meta = document.querySelector('meta[name="broodlink:statusApiUrl"]');
    var apiUrl = (meta && meta.content) || 'http://localhost:3312';
    var keyMeta = document.querySelector('meta[name="broodlink:statusApiKey"]');
    var apiKey = (keyMeta && keyMeta.content) || '';

    var body = { decision: decision, reviewer: 'dashboard-user' };
    if (reason) body.reason = reason;

    fetch(apiUrl + '/api/v1/approvals/' + gateId + '/review', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', 'X-Broodlink-Api-Key': apiKey },
      body: JSON.stringify(body)
    })
    .then(function (resp) { return resp.json(); })
    .then(function () { load(); })
    .catch(function (err) { alert('Review failed: ' + err.message); });
  }

  // -----------------------------------------------------------------------
  // Load data
  // -----------------------------------------------------------------------

  function updateMetrics(approvals, policies) {
    var total = approvals.length;
    var pending = 0, autoApproved = 0;
    approvals.forEach(function (a) {
      if (a.status === 'pending') pending++;
      else if (a.status === 'auto_approved') autoApproved++;
    });
    var activePolicies = policies ? policies.filter(function (p) { return p.active; }).length : 0;
    var el;
    el = document.getElementById('approval-total'); if (el) el.textContent = total;
    el = document.getElementById('approval-pending'); if (el) el.textContent = pending;
    el = document.getElementById('approval-auto'); if (el) el.textContent = autoApproved;
    el = document.getElementById('approval-policies-count'); if (el) el.textContent = activePolicies;
  }

  var allApprovals = [];
  var allPolicies = [];

  function load() {
    Promise.all([
      BL.fetchApi('/api/v1/approvals').catch(function () { return { approvals: [] }; }),
      policiesContainer ? BL.fetchApi('/api/v1/approval-policies').catch(function () { return { policies: [] }; }) : Promise.resolve({ policies: [] })
    ]).then(function (results) {
      allApprovals = results[0].approvals || [];
      allPolicies = results[1].policies || [];
      updateMetrics(allApprovals, allPolicies);

      var filtered = allApprovals;
      var statusVal = filter ? filter.value : '';
      if (statusVal) {
        filtered = allApprovals.filter(function (a) { return a.status === statusVal; });
      }
      renderTable(filtered);
      renderPolicies(allPolicies);
    });
  }

  if (filter) {
    filter.addEventListener('change', load);
  }

  load();
  setInterval(load, BL.REFRESH_INTERVAL || 10000);
})();
