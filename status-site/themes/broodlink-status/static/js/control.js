/*
 * Broodlink - Multi-agent AI orchestration system
 * Copyright (C) 2025-2026 Neven Kordic <neven@broodlink.ai>
 * SPDX-License-Identifier: AGPL-3.0-or-later
 *
 * Control Panel — tabbed admin interface
 */
(function () {
  'use strict';

  var BL = window.Broodlink;
  if (!BL) return;

  // -----------------------------------------------------------------------
  // Toast notifications
  // -----------------------------------------------------------------------
  function toast(message, type) {
    var container = document.getElementById('ctrl-toast-container');
    if (!container) return;
    var el = document.createElement('div');
    el.className = 'ctrl-toast ctrl-toast-' + (type || 'info');
    el.textContent = message;
    container.appendChild(el);
    setTimeout(function () {
      el.style.animation = 'ctrl-toast-out 0.2s ease forwards';
      setTimeout(function () { el.remove(); }, 200);
    }, 3000);
  }

  // -----------------------------------------------------------------------
  // Helpers
  // -----------------------------------------------------------------------
  function badge(status) {
    var cls = 'ctrl-badge ';
    var s = String(status || '').toLowerCase();
    if (s === 'active' || s === 'ok' || s === 'completed' || s === 'delivered') cls += 'ctrl-badge-ok';
    else if (s === 'pending' || s === 'submitted' || s === 'running' || s === 'degraded') cls += 'ctrl-badge-pending';
    else if (s === 'failed' || s === 'error') cls += 'ctrl-badge-failed';
    else if (s === 'claimed' || s === 'in_progress') cls += 'ctrl-badge-claimed';
    else cls += 'ctrl-badge-offline';
    return '<span class="' + cls + '">' + BL.escapeHtml(s || 'unknown') + '</span>';
  }

  function emptyState(icon, title, desc, colspan) {
    var svgMap = {
      agents: '<svg width="40" height="40" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5"><circle cx="12" cy="8" r="4"/><path d="M6 21v-2a4 4 0 014-4h4a4 4 0 014 4v2"/></svg>',
      tasks: '<svg width="40" height="40" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5"><rect x="3" y="3" width="18" height="18" rx="2"/><path d="M9 12l2 2 4-4"/></svg>',
      empty: '<svg width="40" height="40" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5"><path d="M20 7l-8-4-8 4m16 0l-8 4m8-4v10l-8 4m0-10L4 7m8 4v10M4 7v10l8 4"/></svg>'
    };
    var svg = svgMap[icon] || svgMap.empty;
    if (colspan) {
      return '<tr><td colspan="' + colspan + '"><div class="ctrl-empty">' +
        svg + '<div class="ctrl-empty-title">' + BL.escapeHtml(title) + '</div>' +
        '<div class="ctrl-empty-desc">' + BL.escapeHtml(desc) + '</div></div></td></tr>';
    }
    return '<div class="ctrl-empty">' + svg +
      '<div class="ctrl-empty-title">' + BL.escapeHtml(title) + '</div>' +
      '<div class="ctrl-empty-desc">' + BL.escapeHtml(desc) + '</div></div>';
  }

  function budgetColor(pct) {
    if (pct > 50) return 'var(--green)';
    if (pct > 20) return 'var(--amber)';
    return 'var(--red)';
  }

  // -----------------------------------------------------------------------
  // Tab switching
  // -----------------------------------------------------------------------
  var tabs = document.querySelectorAll('.tab-bar .tab');
  var panels = document.querySelectorAll('[role="tabpanel"]');

  function switchTab(tabName) {
    tabs.forEach(function (t) {
      var active = t.dataset.tab === tabName;
      t.classList.toggle('active', active);
      t.setAttribute('aria-selected', String(active));
    });
    panels.forEach(function (p) {
      p.hidden = p.id !== 'panel-' + tabName;
    });
    loadTab(tabName);
  }

  tabs.forEach(function (t) {
    t.addEventListener('click', function () {
      switchTab(t.dataset.tab);
    });
  });

  // -----------------------------------------------------------------------
  // API helpers
  // -----------------------------------------------------------------------
  function postApi(path, body) {
    var hdrs = {
      'Content-Type': 'application/json',
      'X-Broodlink-Api-Key': BL.STATUS_API_KEY
    };
    var sessionToken = typeof sessionStorage !== 'undefined'
      ? sessionStorage.getItem('broodlink_session_token')
      : null;
    if (sessionToken) hdrs['X-Broodlink-Session'] = sessionToken;

    return fetch(BL.STATUS_API_URL + path, {
      method: 'POST',
      headers: hdrs,
      body: body ? JSON.stringify(body) : undefined
    }).then(function (res) {
      if (!res.ok) return res.json().then(function (e) { throw new Error(e.error || 'request failed'); });
      return res.json();
    }).then(function (json) {
      return json.data !== undefined ? json.data : json;
    });
  }

  // -----------------------------------------------------------------------
  // Summary metrics
  // -----------------------------------------------------------------------
  function loadMetrics() {
    BL.fetchApi('/api/v1/agents').then(function (data) {
      var agents = data.agents || data || [];
      var active = agents.filter(function (a) { return a.active; }).length;
      var el = document.getElementById('ctrl-metric-agents');
      if (el) el.textContent = active + '/' + agents.length;
    }).catch(function () {});

    BL.fetchApi('/api/v1/budgets').then(function (data) {
      var budgets = data.budgets || [];
      var total = budgets.reduce(function (s, b) { return s + (b.budget_tokens || 0); }, 0);
      var el = document.getElementById('ctrl-metric-budget');
      if (el) el.textContent = total.toLocaleString();
    }).catch(function () {});

    BL.fetchApi('/api/v1/tasks').then(function (data) {
      var counts = data.counts_by_status || {};
      var pending = counts.pending || 0;
      var el = document.getElementById('ctrl-metric-tasks');
      if (el) el.textContent = String(pending);
    }).catch(function () {});

    BL.fetchApi('/api/v1/dlq').then(function (data) {
      var entries = data.entries || [];
      var el = document.getElementById('ctrl-metric-dlq');
      if (el) {
        el.textContent = String(entries.length);
        if (entries.length > 0) el.style.color = 'var(--red)';
        else el.style.color = '';
      }
    }).catch(function () {});
  }

  // -----------------------------------------------------------------------
  // Agents tab
  // -----------------------------------------------------------------------
  function loadAgents() {
    BL.fetchApi('/api/v1/agents').then(function (data) {
      var tbody = document.getElementById('ctrl-agents-tbody');
      if (!tbody) return;
      var agents = data.agents || data || [];
      if (agents.length === 0) {
        tbody.innerHTML = emptyState('agents', 'No agents registered', 'Register agents via the MCP server or beads-bridge.', 6);
        return;
      }
      tbody.innerHTML = agents.map(function (a) {
        var statusBadge = badge(a.active ? 'active' : 'offline');
        var tierBadge = '<span class="ctrl-badge ctrl-badge-offline">' + BL.escapeHtml(a.cost_tier || 'medium') + '</span>';
        return '<tr>' +
          '<td><strong>' + BL.escapeHtml(a.agent_id) + '</strong>' +
          (a.display_name && a.display_name !== a.agent_id ? '<br><small style="color:var(--text-secondary)">' + BL.escapeHtml(a.display_name) + '</small>' : '') + '</td>' +
          '<td>' + BL.escapeHtml(a.role || '—') + '</td>' +
          '<td>' + tierBadge + '</td>' +
          '<td><code>' + BL.escapeHtml(a.transport || 'mcp') + '</code></td>' +
          '<td>' + statusBadge + '</td>' +
          '<td><div class="ctrl-btn-group">' +
          '<button class="btn btn-sm ' + (a.active ? 'btn-danger' : 'btn-primary') + '" onclick="Ctrl.toggleAgent(\'' + BL.escapeHtml(a.agent_id) + '\')">' +
          (a.active ? 'Deactivate' : 'Activate') + '</button>' +
          '</div></td>' +
          '</tr>';
      }).join('');
    }).catch(function () {
      var el = document.getElementById('ctrl-agents-tbody');
      if (el) el.innerHTML = '<tr><td colspan="6" style="color:var(--red)">Failed to load agents.</td></tr>';
    });
  }

  function toggleAgent(agentId) {
    if (!confirm('Toggle active state for agent "' + agentId + '"?')) return;
    postApi('/api/v1/agents/' + agentId + '/toggle')
      .then(function () {
        toast('Agent "' + agentId + '" toggled', 'success');
        loadAgents();
        loadMetrics();
      })
      .catch(function (err) { toast('Toggle failed: ' + err.message, 'error'); });
  }

  // -----------------------------------------------------------------------
  // Budgets tab (card-based layout)
  // -----------------------------------------------------------------------
  var BUDGET_MAX = 100000; // default scale for progress bar

  function loadBudgets() {
    BL.fetchApi('/api/v1/budgets').then(function (data) {
      var container = document.getElementById('ctrl-budgets-cards');
      if (!container) return;
      var budgets = data.budgets || [];
      if (budgets.length === 0) {
        container.innerHTML = emptyState('empty', 'No budget data', 'Budget tracking starts when agents make tool calls.');
        return;
      }
      // Determine max for relative progress bars
      var maxBudget = budgets.reduce(function (m, b) { return Math.max(m, b.budget_tokens || 0); }, BUDGET_MAX);

      container.innerHTML = budgets.map(function (b) {
        var tokens = b.budget_tokens || 0;
        var pct = Math.min(100, Math.round((tokens / maxBudget) * 100));
        var color = budgetColor(pct);
        return '<div class="ctrl-budget-card">' +
          '<div class="ctrl-budget-card-header">' +
          '<span class="agent-name">' + BL.escapeHtml(b.agent_id) + '</span>' +
          '</div>' +
          '<div class="ctrl-budget-bar"><div class="ctrl-budget-bar-fill" style="width:' + pct + '%;background:' + color + '"></div></div>' +
          '<div class="ctrl-budget-card-footer">' +
          '<div><span class="ctrl-budget-amount">' + tokens.toLocaleString() + '</span> <span class="ctrl-budget-label">tokens</span></div>' +
          '<button class="btn btn-sm btn-ghost" onclick="Ctrl.setBudget(\'' + BL.escapeHtml(b.agent_id) + '\', ' + tokens + ')">Set Budget</button>' +
          '</div></div>';
      }).join('');
    }).catch(function () {
      var el = document.getElementById('ctrl-budgets-cards');
      if (el) el.innerHTML = '<div style="color:var(--red)">Failed to load budgets.</div>';
    });
  }

  function setBudget(agentId, current) {
    var input = prompt('Set budget for ' + agentId + ' (current: ' + Number(current).toLocaleString() + '):', String(current));
    if (input === null) return;
    var tokens = parseInt(input, 10);
    if (isNaN(tokens) || tokens < 0) { toast('Invalid number', 'error'); return; }
    postApi('/api/v1/budgets/' + agentId + '/set', { tokens: tokens })
      .then(function () {
        toast('Budget set to ' + tokens.toLocaleString() + ' for ' + agentId, 'success');
        loadBudgets();
        loadMetrics();
      })
      .catch(function (err) { toast('Set failed: ' + err.message, 'error'); });
  }

  // -----------------------------------------------------------------------
  // Tasks tab
  // -----------------------------------------------------------------------
  function loadTasks() {
    BL.fetchApi('/api/v1/tasks').then(function (data) {
      var tbody = document.getElementById('ctrl-tasks-tbody');
      if (!tbody) return;
      var tasks = data.recent || [];
      if (tasks.length === 0) {
        tbody.innerHTML = emptyState('tasks', 'No active tasks', 'Tasks appear when agents submit work.', 6);
        return;
      }
      tbody.innerHTML = tasks.map(function (t) {
        var canCancel = t.status === 'pending' || t.status === 'claimed';
        return '<tr>' +
          '<td><code>' + BL.escapeHtml((t.id || '').slice(0, 8)) + '</code></td>' +
          '<td>' + BL.escapeHtml(t.title || '') + '</td>' +
          '<td>' + badge(t.status) + '</td>' +
          '<td>' + BL.escapeHtml(t.assigned_agent || '—') + '</td>' +
          '<td>' + BL.formatRelativeTime(t.created_at) + '</td>' +
          '<td>' + (canCancel
            ? '<div class="ctrl-btn-group"><button class="btn btn-sm btn-danger" onclick="Ctrl.cancelTask(\'' + BL.escapeHtml(t.id) + '\')">Cancel</button></div>'
            : '') + '</td>' +
          '</tr>';
      }).join('');
    }).catch(function () {
      var el = document.getElementById('ctrl-tasks-tbody');
      if (el) el.innerHTML = '<tr><td colspan="6" style="color:var(--red)">Failed to load tasks.</td></tr>';
    });
  }

  function cancelTask(taskId) {
    if (!confirm('Cancel task ' + taskId.slice(0, 8) + '?')) return;
    postApi('/api/v1/tasks/' + taskId + '/cancel')
      .then(function () {
        toast('Task ' + taskId.slice(0, 8) + ' cancelled', 'success');
        loadTasks();
        loadMetrics();
      })
      .catch(function (err) { toast('Cancel failed: ' + err.message, 'error'); });
  }

  // -----------------------------------------------------------------------
  // Workflows tab
  // -----------------------------------------------------------------------
  function loadWorkflows() {
    BL.fetchApi('/api/v1/workflows').then(function (data) {
      var tbody = document.getElementById('ctrl-workflows-tbody');
      if (!tbody) return;
      var workflows = data.workflows || [];
      if (workflows.length === 0) {
        tbody.innerHTML = emptyState('empty', 'No workflow runs', 'Trigger a formula to start a workflow.', 6);
        return;
      }
      tbody.innerHTML = workflows.map(function (w) {
        var current = w.current_step || 0;
        var total = w.total_steps || 1;
        var pct = Math.round((current / total) * 100);
        return '<tr>' +
          '<td><code>' + BL.escapeHtml((w.id || '').slice(0, 8)) + '</code></td>' +
          '<td>' + BL.escapeHtml(w.formula_name || '') + '</td>' +
          '<td>' + badge(w.status) + '</td>' +
          '<td><div class="ctrl-progress">' +
          '<div class="ctrl-progress-bar"><div class="ctrl-progress-bar-fill" style="width:' + pct + '%"></div></div>' +
          '<span class="ctrl-progress-label">' + current + '/' + total + '</span></div></td>' +
          '<td>' + BL.escapeHtml(w.started_by || '') + '</td>' +
          '<td>' + BL.formatRelativeTime(w.created_at) + '</td>' +
          '</tr>';
      }).join('');
    }).catch(function () {
      var el = document.getElementById('ctrl-workflows-tbody');
      if (el) el.innerHTML = '<tr><td colspan="6" style="color:var(--red)">Failed to load workflows.</td></tr>';
    });
  }

  // -----------------------------------------------------------------------
  // Guardrails tab
  // -----------------------------------------------------------------------
  function loadGuardrails() {
    BL.fetchApi('/api/v1/guardrails').then(function (data) {
      var tbody = document.getElementById('ctrl-guardrails-tbody');
      if (!tbody) return;
      var policies = data.policies || data || [];
      if (policies.length === 0) {
        tbody.innerHTML = emptyState('empty', 'No guardrail policies', 'Create policies to enforce safety constraints.', 4);
        return;
      }
      tbody.innerHTML = policies.map(function (p) {
        return '<tr>' +
          '<td><strong>' + BL.escapeHtml(p.name || p.policy_name || '') + '</strong></td>' +
          '<td><code>' + BL.escapeHtml(p.rule_type || '') + '</code></td>' +
          '<td>' + badge(p.enabled ? 'active' : 'offline') + '</td>' +
          '<td>' + (p.violation_count || 0) + '</td>' +
          '</tr>';
      }).join('');
    }).catch(function () {
      var el = document.getElementById('ctrl-guardrails-tbody');
      if (el) el.innerHTML = '<tr><td colspan="4" style="color:var(--red)">Failed to load.</td></tr>';
    });

    BL.fetchApi('/api/v1/violations?limit=10').then(function (data) {
      var tbody = document.getElementById('ctrl-violations-tbody');
      if (!tbody) return;
      var violations = data.violations || data || [];
      if (violations.length === 0) {
        tbody.innerHTML = emptyState('tasks', 'No violations', 'Clean record — no guardrail violations detected.', 4);
        return;
      }
      tbody.innerHTML = violations.map(function (v) {
        return '<tr>' +
          '<td>' + BL.escapeHtml(v.agent_id || '') + '</td>' +
          '<td>' + BL.escapeHtml(v.policy_name || '') + '</td>' +
          '<td><code>' + BL.escapeHtml(v.tool_name || '') + '</code></td>' +
          '<td>' + BL.formatRelativeTime(v.created_at) + '</td>' +
          '</tr>';
      }).join('');
    }).catch(function () {
      var el = document.getElementById('ctrl-violations-tbody');
      if (el) el.innerHTML = '<tr><td colspan="4" style="color:var(--red)">Failed to load.</td></tr>';
    });
  }

  // -----------------------------------------------------------------------
  // DLQ tab
  // -----------------------------------------------------------------------
  function loadDlq() {
    BL.fetchApi('/api/v1/dlq').then(function (data) {
      var tbody = document.getElementById('ctrl-dlq-tbody');
      if (!tbody) return;
      var entries = data.entries || [];
      if (entries.length === 0) {
        tbody.innerHTML = emptyState('tasks', 'Queue is empty', 'No failed tasks in the dead-letter queue.', 6);
        return;
      }
      tbody.innerHTML = entries.map(function (e) {
        return '<tr>' +
          '<td>' + e.id + '</td>' +
          '<td><code>' + BL.escapeHtml((e.task_id || '').slice(0, 8)) + '</code></td>' +
          '<td>' + BL.escapeHtml(e.reason || '') + '</td>' +
          '<td><span class="ctrl-progress-label">' + e.retry_count + '/' + e.max_retries + '</span></td>' +
          '<td>' + BL.formatRelativeTime(e.created_at) + '</td>' +
          '<td>' + (e.resolved
            ? badge('completed')
            : '<div class="ctrl-btn-group"><button class="btn btn-sm btn-ghost" onclick="Ctrl.retryDlq(' + e.id + ')">Retry</button></div>')
          + '</td>' +
          '</tr>';
      }).join('');
    }).catch(function () {
      var el = document.getElementById('ctrl-dlq-tbody');
      if (el) el.innerHTML = '<tr><td colspan="6" style="color:var(--red)">Failed to load.</td></tr>';
    });
  }

  function retryDlq(dlqId) {
    toast('Manual DLQ retry: use retry_dlq_task tool with dlq_id=' + dlqId, 'info');
  }

  // -----------------------------------------------------------------------
  // Webhooks tab
  // -----------------------------------------------------------------------
  function loadWebhooks() {
    BL.fetchApi('/api/v1/webhooks').then(function (data) {
      var tbody = document.getElementById('ctrl-webhooks-tbody');
      if (!tbody) return;
      var endpoints = data.endpoints || [];
      if (endpoints.length === 0) {
        tbody.innerHTML = emptyState('empty', 'No webhook endpoints', 'Add an endpoint to receive notifications.', 6);
      } else {
        tbody.innerHTML = endpoints.map(function (ep) {
          var evts = Array.isArray(ep.events) ? ep.events.join(', ') : '';
          var urlDisplay = ep.webhook_url ? BL.escapeHtml(ep.webhook_url.slice(0, 40)) : '—';
          return '<tr>' +
            '<td><strong>' + BL.escapeHtml(ep.name) + '</strong></td>' +
            '<td>' + badge(ep.platform) + '</td>' +
            '<td><code style="font-size:0.75rem">' + urlDisplay + '</code></td>' +
            '<td><span style="font-size:0.75rem">' + BL.escapeHtml(evts || 'all') + '</span></td>' +
            '<td>' + badge(ep.active ? 'active' : 'offline') + '</td>' +
            '<td><div class="ctrl-btn-group">' +
            '<button class="btn btn-sm btn-ghost" onclick="Ctrl.toggleWebhook(\'' + BL.escapeHtml(ep.id) + '\')">' +
            (ep.active ? 'Disable' : 'Enable') + '</button>' +
            '<button class="btn btn-sm btn-danger" onclick="Ctrl.deleteWebhook(\'' + BL.escapeHtml(ep.id) + '\')">Delete</button>' +
            '</div></td></tr>';
        }).join('');
      }
    }).catch(function () {
      var el = document.getElementById('ctrl-webhooks-tbody');
      if (el) el.innerHTML = '<tr><td colspan="6" style="color:var(--red)">Failed to load.</td></tr>';
    });

    BL.fetchApi('/api/v1/webhook-log?limit=20').then(function (data) {
      var tbody = document.getElementById('ctrl-webhook-log-tbody');
      if (!tbody) return;
      var entries = data.entries || [];
      if (entries.length === 0) {
        tbody.innerHTML = emptyState('empty', 'No deliveries yet', 'Webhook delivery logs will appear here.', 5);
        return;
      }
      tbody.innerHTML = entries.map(function (e) {
        var dirBadge = e.direction === 'inbound'
          ? '<span class="ctrl-badge ctrl-badge-claimed">inbound</span>'
          : '<span class="ctrl-badge ctrl-badge-pending">outbound</span>';
        return '<tr>' +
          '<td>' + e.id + '</td>' +
          '<td>' + dirBadge + '</td>' +
          '<td><code>' + BL.escapeHtml(e.event_type) + '</code></td>' +
          '<td>' + badge(e.status) + '</td>' +
          '<td>' + BL.formatRelativeTime(e.created_at) + '</td>' +
          '</tr>';
      }).join('');
    }).catch(function () {
      var el = document.getElementById('ctrl-webhook-log-tbody');
      if (el) el.innerHTML = '<tr><td colspan="5" style="color:var(--red)">Failed to load.</td></tr>';
    });
  }

  function addWebhook() {
    var name = prompt('Webhook name:');
    if (!name) return;
    var platform = prompt('Platform (slack, teams, telegram, generic):', 'generic');
    if (!platform) return;
    var url = prompt('Outbound webhook URL (leave blank for inbound-only):');
    var events = prompt('Subscribe to events (comma-separated, blank = all):', '');
    var eventList = events ? events.split(',').map(function (e) { return e.trim(); }) : [];

    postApi('/api/v1/webhooks', {
      name: name,
      platform: platform,
      webhook_url: url || null,
      events: eventList,
    })
    .then(function () {
      toast('Webhook "' + name + '" created', 'success');
      loadWebhooks();
    })
    .catch(function (err) { toast('Create failed: ' + err.message, 'error'); });
  }

  function toggleWebhook(endpointId) {
    postApi('/api/v1/webhooks/' + endpointId + '/toggle')
      .then(function () {
        toast('Webhook toggled', 'success');
        loadWebhooks();
      })
      .catch(function (err) { toast('Toggle failed: ' + err.message, 'error'); });
  }

  function deleteWebhook(endpointId) {
    if (!confirm('Delete webhook endpoint?')) return;
    postApi('/api/v1/webhooks/' + endpointId + '/delete')
      .then(function () {
        toast('Webhook deleted', 'success');
        loadWebhooks();
      })
      .catch(function (err) { toast('Delete failed: ' + err.message, 'error'); });
  }

  // -----------------------------------------------------------------------
  // Tab loader dispatcher
  // -----------------------------------------------------------------------
  // Chat sessions (v0.7.0)
  // -----------------------------------------------------------------------
  function loadChat() {
    var tbody = document.getElementById('ctrl-chat-tbody');
    if (!tbody) return;
    BL.fetchApi('/api/v1/chat/sessions?status=active').then(function (data) {
      var sessions = data.sessions || [];
      if (sessions.length === 0) {
        tbody.innerHTML = '<tr><td colspan="7">No active chat sessions.</td></tr>';
        return;
      }
      tbody.innerHTML = sessions.map(function (s) {
        var name = BL.escapeHtml(s.user_display_name || s.user_id || '?');
        var agent = s.assigned_agent ? BL.escapeHtml(s.assigned_agent) : '<em>auto</em>';
        var lastMsg = s.last_message_at ? BL.formatRelativeTime(s.last_message_at) : '—';
        return '<tr>' +
          '<td>' + BL.escapeHtml(s.platform) + '</td>' +
          '<td>' + name + '</td>' +
          '<td>' + s.message_count + '</td>' +
          '<td>' + agent + '</td>' +
          '<td>' + lastMsg + '</td>' +
          '<td>' + badge(s.status === 'active' ? 'ok' : 'offline', s.status) + '</td>' +
          '<td>' +
            (s.status === 'active' ? '<button class="btn btn-sm btn-danger" onclick="Ctrl.closeChat(\'' + BL.escapeHtml(s.id) + '\')">Close</button>' : '') +
          '</td>' +
        '</tr>';
      }).join('');
    }).catch(function () {
      tbody.innerHTML = '<tr><td colspan="7">Failed to load chat sessions.</td></tr>';
    });
  }

  function closeChat(sessionId) {
    BL.fetchApi('/api/v1/chat/sessions/' + sessionId + '/close', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: '{}'
    }).then(function () {
      toast('Session closed', 'ok');
      loadChat();
    }).catch(function (e) {
      toast('Failed to close session: ' + e, 'error');
    });
  }

  // -----------------------------------------------------------------------
  // Formulas tab
  // -----------------------------------------------------------------------
  function loadFormulas() {
    BL.fetchApi('/api/v1/formulas').then(function (data) {
      var tbody = document.getElementById('ctrl-formulas-tbody');
      if (!tbody) return;
      var formulas = data.formulas || [];
      if (formulas.length === 0) {
        tbody.innerHTML = '<tr><td colspan="7">No formulas registered.</td></tr>';
        return;
      }
      tbody.innerHTML = formulas.map(function (f) {
        var statusCls = f.enabled ? 'ok' : 'offline';
        var statusLabel = f.enabled ? 'Enabled' : 'Disabled';
        var systemBadge = f.is_system ? '<span class="badge badge-info">system</span>' : '';
        var editLabel = f.is_system ? 'View' : 'Edit';
        return '<tr>' +
          '<td><strong>' + BL.escapeHtml(f.display_name || f.name) + '</strong><br><code>' + BL.escapeHtml(f.name) + '</code></td>' +
          '<td>' + BL.escapeHtml(f.description || '—') + '</td>' +
          '<td>v' + f.version + '</td>' +
          '<td>' + f.usage_count + '</td>' +
          '<td>' + systemBadge + '</td>' +
          '<td><span class="badge badge-' + statusCls + '">' + statusLabel + '</span></td>' +
          '<td>' +
            '<button class="btn btn-sm" onclick="Ctrl.toggleFormula(\'' + BL.escapeHtml(f.name) + '\')">' + (f.enabled ? 'Disable' : 'Enable') + '</button> ' +
            '<button class="btn btn-sm" onclick="Ctrl.viewFormula(\'' + BL.escapeHtml(f.name) + '\')">' + editLabel + '</button>' +
          '</td>' +
        '</tr>';
      }).join('');
    }).catch(function () {});
  }

  function toggleFormula(name) {
    BL.fetchApi('/api/v1/formulas/' + name + '/toggle', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: '{}'
    }).then(function (data) {
      toast('Formula ' + name + ' ' + (data.enabled ? 'enabled' : 'disabled'), 'ok');
      loadFormulas();
    }).catch(function (e) {
      toast('Failed to toggle formula: ' + e, 'error');
    });
  }

  function viewFormula(name) {
    BL.fetchApi('/api/v1/formulas/' + name).then(function (data) {
      if (window.FormulaEditor) {
        window.FormulaEditor.open(data);
      } else {
        // Fallback if editor JS not loaded
        var defStr = JSON.stringify(data.definition, null, 2);
        alert('Formula: ' + name + '\n\nDefinition:\n' + defStr);
      }
    }).catch(function (e) {
      toast('Failed to load formula: ' + e, 'error');
    });
  }

  function createFormula() {
    if (window.FormulaEditor) {
      window.FormulaEditor.openNew();
    } else {
      toast('Formula editor not available', 'error');
    }
  }

  // -----------------------------------------------------------------------
  // Users tab (admin only)
  // -----------------------------------------------------------------------
  function loadUsers() {
    BL.fetchApi('/api/v1/users').then(function (data) {
      var tbody = document.getElementById('ctrl-users-tbody');
      if (!tbody) return;
      var users = data.users || [];
      if (users.length === 0) {
        tbody.innerHTML = emptyState('agents', 'No dashboard users', 'Create an admin user with scripts/create-admin.sh.', 7);
        return;
      }
      tbody.innerHTML = users.map(function (u) {
        var statusBadge = badge(u.active ? 'active' : 'offline');
        var lastLogin = u.last_login ? BL.formatRelativeTime(u.last_login) : 'never';
        return '<tr>' +
          '<td><strong>' + BL.escapeHtml(u.username) + '</strong>' +
          (u.display_name ? '<br><small style="color:var(--text-secondary)">' + BL.escapeHtml(u.display_name) + '</small>' : '') + '</td>' +
          '<td>' + badge(u.role) + '</td>' +
          '<td>' + statusBadge + '</td>' +
          '<td>' + lastLogin + '</td>' +
          '<td>' + BL.escapeHtml(u.created_at || '') + '</td>' +
          '<td><div class="ctrl-btn-group">' +
          '<button class="btn btn-sm btn-ghost" onclick="Ctrl.changeRole(\'' + BL.escapeHtml(u.id) + '\', \'' + BL.escapeHtml(u.role) + '\')">Role</button>' +
          '<button class="btn btn-sm ' + (u.active ? 'btn-danger' : 'btn-primary') + '" onclick="Ctrl.toggleUser(\'' + BL.escapeHtml(u.id) + '\')">' +
          (u.active ? 'Deactivate' : 'Activate') + '</button>' +
          '<button class="btn btn-sm btn-ghost" onclick="Ctrl.resetPassword(\'' + BL.escapeHtml(u.id) + '\')">Reset PW</button>' +
          '</div></td>' +
          '</tr>';
      }).join('');
    }).catch(function () {
      var el = document.getElementById('ctrl-users-tbody');
      if (el) el.innerHTML = '<tr><td colspan="6" style="color:var(--red)">Failed to load users.</td></tr>';
    });
  }

  function createUser() {
    var username = prompt('Username:');
    if (!username) return;
    var password = prompt('Password:');
    if (!password) return;
    var role = prompt('Role (viewer, operator, admin):', 'viewer');
    if (!role) return;
    var displayName = prompt('Display name (optional):') || null;

    postApi('/api/v1/users', {
      username: username,
      password: password,
      role: role,
      display_name: displayName
    }).then(function () {
      toast('User "' + username + '" created', 'success');
      loadUsers();
    }).catch(function (err) {
      toast('Create failed: ' + err.message, 'error');
    });
  }

  function changeRole(userId, currentRole) {
    var newRole = prompt('New role (viewer, operator, admin). Current: ' + currentRole, currentRole);
    if (!newRole || newRole === currentRole) return;
    postApi('/api/v1/users/' + userId + '/role', { role: newRole })
      .then(function () {
        toast('Role updated to ' + newRole, 'success');
        loadUsers();
      })
      .catch(function (err) { toast('Role change failed: ' + err.message, 'error'); });
  }

  function toggleUser(userId) {
    if (!confirm('Toggle active state for this user?')) return;
    postApi('/api/v1/users/' + userId + '/toggle')
      .then(function () {
        toast('User toggled', 'success');
        loadUsers();
      })
      .catch(function (err) { toast('Toggle failed: ' + err.message, 'error'); });
  }

  function resetPassword(userId) {
    var newPw = prompt('New password:');
    if (!newPw) return;
    postApi('/api/v1/users/' + userId + '/reset-password', { password: newPw })
      .then(function () {
        toast('Password reset', 'success');
      })
      .catch(function (err) { toast('Reset failed: ' + err.message, 'error'); });
  }

  // -----------------------------------------------------------------------
  // Role-based UI enforcement
  // -----------------------------------------------------------------------
  function enforceRoles() {
    var Auth = window.BLAuth;
    if (!Auth) return;

    // Hide Users tab if not admin
    var usersTab = document.querySelector('[data-tab="users"]');
    if (usersTab && !Auth.isAdmin()) {
      usersTab.style.display = 'none';
    }

    // Hide write buttons if viewer-only
    if (!Auth.canWrite()) {
      var writeButtons = document.querySelectorAll('.ctrl-write-action');
      writeButtons.forEach(function (btn) {
        btn.style.display = 'none';
      });
    }
  }

  // -----------------------------------------------------------------------
  // Telegram bot registration
  // -----------------------------------------------------------------------
  function loadTelegram() {
    var container = document.getElementById('ctrl-telegram-content');
    if (!container) return;

    // Don't re-render if the registration form is already showing (preserves user input)
    if (document.getElementById('tg-bot-token')) return;

    BL.fetchApi('/api/v1/telegram/status').then(function (data) {
      if (data.configured) {
        container.innerHTML =
          '<div class="card" style="max-width:480px;padding:1.5rem">' +
          '<div style="display:flex;align-items:center;gap:0.75rem;margin-bottom:1rem">' +
          '<svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="var(--green)" stroke-width="2"><path d="M22 11.08V12a10 10 0 11-5.93-9.14"/><polyline points="22 4 12 14.01 9 11.01"/></svg>' +
          '<strong style="font-size:1.1rem">Connected</strong>' +
          '</div>' +
          '<table style="width:100%;font-size:0.85rem">' +
          '<tr><td style="color:var(--muted);padding:0.25rem 1rem 0.25rem 0">Bot</td>' +
          '<td><strong>' + BL.escapeHtml(data.bot_username || '—') + '</strong></td></tr>' +
          '<tr><td style="color:var(--muted);padding:0.25rem 1rem 0.25rem 0">Mode</td>' +
          '<td>' + badge(data.mode === 'polling' ? 'active' : 'pending') + ' ' + BL.escapeHtml(data.mode || 'unknown') + '</td></tr>' +
          '<tr><td style="color:var(--muted);padding:0.25rem 1rem 0.25rem 0">Registered</td>' +
          '<td>' + BL.formatRelativeTime(data.registered_at) + '</td></tr>' +
          '<tr><td style="color:var(--muted);padding:0.25rem 1rem 0.25rem 0">Status</td>' +
          '<td>' + badge(data.enabled ? 'active' : 'offline') + '</td></tr>' +
          (data.auth_code ? '<tr><td style="color:var(--muted);padding:0.25rem 1rem 0.25rem 0">Access Code</td>' +
          '<td><code style="background:var(--bg);padding:0.15rem 0.5rem;border-radius:3px;font-size:0.9rem;letter-spacing:0.05em">' +
          BL.escapeHtml(data.auth_code) + '</code></td></tr>' : '') +
          (data.allowed_users && data.allowed_users.length > 0 ? '<tr><td style="color:var(--muted);padding:0.25rem 1rem 0.25rem 0">Authorized</td>' +
          '<td>' + data.allowed_users.length + ' user' + (data.allowed_users.length !== 1 ? 's' : '') + '</td></tr>' : '') +
          '</table>' +
          '<div style="margin-top:1.25rem;display:flex;gap:0.5rem">' +
          '<button class="btn btn-danger btn-sm" onclick="Ctrl.disconnectBot()">Disconnect Bot</button>' +
          '</div></div>';
      } else {
        container.innerHTML =
          '<div class="card" style="max-width:480px;padding:1.5rem">' +
          '<p style="margin-bottom:1rem;color:var(--muted)">Register your Telegram bot to receive and reply to messages. ' +
          'Uses long-polling by default (no public URL needed).</p>' +
          '<div style="display:flex;flex-direction:column;gap:0.75rem">' +
          '<label style="font-size:0.85rem;font-weight:600">Bot Token' +
          '<input id="tg-bot-token" type="text" placeholder="Paste full token from @BotFather" ' +
          'style="width:100%;margin-top:0.25rem;padding:0.5rem;background:var(--bg-card);border:1px solid var(--border);border-radius:4px;color:var(--fg);font-family:monospace;font-size:0.8rem" />' +
          '</label>' +
          '</div>' +
          '<div style="margin-top:1.25rem">' +
          '<button class="btn btn-primary" id="tg-register-btn" onclick="Ctrl.registerBot()">Register Bot</button>' +
          '</div></div>';
      }
    }).catch(function () {
      container.innerHTML = '<p style="color:var(--red)">Failed to load Telegram status.</p>';
    });
  }

  function registerBot() {
    var token = (document.getElementById('tg-bot-token') || {}).value || '';

    if (!token) { toast('Bot token is required', 'error'); return; }

    var btn = document.getElementById('tg-register-btn');
    if (btn) { btn.disabled = true; btn.textContent = 'Registering...'; }

    var payload = { bot_token: token };

    postApi('/api/v1/telegram/register', payload)
      .then(function (data) {
        toast('Bot registered as ' + (data.bot_username || 'unknown'), 'success');
        // Clear the form so loadTelegram's guard doesn't bail
        var c = document.getElementById('ctrl-telegram-content');
        if (c) c.innerHTML = '';
        loadTelegram();
      })
      .catch(function (err) {
        toast('Registration failed: ' + err.message, 'error');
        if (btn) { btn.disabled = false; btn.textContent = 'Register Bot'; }
      });
  }

  function disconnectBot() {
    if (!confirm('Disconnect the Telegram bot? This will remove the webhook, credentials, and all chat history.')) return;
    postApi('/api/v1/telegram/disconnect')
      .then(function () {
        toast('Telegram bot disconnected', 'success');
        loadTelegram();
      })
      .catch(function (err) { toast('Disconnect failed: ' + err.message, 'error'); });
  }

  // -----------------------------------------------------------------------
  // Services
  // -----------------------------------------------------------------------
  function severityBadge(severity) {
    var s = String(severity || '').toLowerCase();
    var cls = 'ctrl-badge ';
    if (s === 'info') cls += 'ctrl-badge-ok';
    else if (s === 'warning') cls += 'ctrl-badge-pending';
    else if (s === 'error') cls += 'ctrl-badge-failed';
    else cls += 'ctrl-badge-offline';
    return '<span class="' + cls + '">' + BL.escapeHtml(s || 'unknown') + '</span>';
  }

  function formatEventDetails(details) {
    if (!details || typeof details !== 'object') return '—';
    var parts = [];
    Object.keys(details).forEach(function (k) {
      parts.push(BL.escapeHtml(k) + '=' + BL.escapeHtml(String(details[k])));
    });
    return parts.length ? '<small>' + parts.join(', ') + '</small>' : '—';
  }

  function loadServices() {
    BL.fetchApi('/api/v1/services').then(function (data) {
      var services = (data && data.services) || [];
      var tb = document.getElementById('ctrl-services-tbody');
      if (!tb) return;
      tb.innerHTML = services.map(function (svc) {
        var details = svc.details || {};
        var modelCell = '';
        if (svc.name === 'a2a-gateway' && details.model_degraded) {
          modelCell = '<span style="color:var(--amber)">' +
            BL.escapeHtml(details.active_model || '?') + '</span>' +
            ' <small style="color:var(--text-secondary)">(primary: ' +
            BL.escapeHtml(details.chat_model || '?') + ')</small>';
        } else if (svc.name === 'a2a-gateway' && details.active_model) {
          modelCell = BL.escapeHtml(details.active_model);
        } else {
          modelCell = '<span style="color:var(--text-secondary)">—</span>';
        }
        var latency = svc.latency_ms != null ? svc.latency_ms + ' ms' : '—';
        var checkedAt = svc.checked_at ? new Date(svc.checked_at).toLocaleTimeString() : '—';
        return '<tr>' +
          '<td>' + BL.escapeHtml(svc.name) + '</td>' +
          '<td>' + svc.port + '</td>' +
          '<td>' + badge(svc.status) + '</td>' +
          '<td>' + modelCell + '</td>' +
          '<td>' + latency + '</td>' +
          '<td>' + checkedAt + '</td>' +
          '</tr>';
      }).join('');
    }).catch(function (err) {
      var tb = document.getElementById('ctrl-services-tbody');
      if (tb) tb.innerHTML = '<tr><td colspan="6" style="color:var(--red)">Failed to load services: ' + BL.escapeHtml(err.message) + '</td></tr>';
    });

    // Load service events
    BL.fetchApi('/api/v1/services/events?limit=20').then(function (data) {
      var events = (data && data.events) || [];
      var tb = document.getElementById('ctrl-service-events-tbody');
      if (!tb) return;
      if (!events.length) {
        tb.innerHTML = emptyState('empty', 'No Events', 'No service events recorded yet.', 5);
        return;
      }
      tb.innerHTML = events.map(function (evt) {
        var time = evt.created_at ? new Date(evt.created_at).toLocaleString() : '—';
        return '<tr>' +
          '<td>' + BL.escapeHtml(time) + '</td>' +
          '<td>' + BL.escapeHtml(evt.service || '') + '</td>' +
          '<td>' + BL.escapeHtml(evt.event_type || '') + '</td>' +
          '<td>' + severityBadge(evt.severity) + '</td>' +
          '<td>' + formatEventDetails(evt.details) + '</td>' +
          '</tr>';
      }).join('');
    }).catch(function (err) {
      var tb = document.getElementById('ctrl-service-events-tbody');
      if (tb) tb.innerHTML = '<tr><td colspan="5" style="color:var(--red)">Failed to load events: ' + BL.escapeHtml(err.message) + '</td></tr>';
    });
  }

  // -----------------------------------------------------------------------
  function loadTab(name) {
    switch (name) {
      case 'agents': loadAgents(); break;
      case 'budgets': loadBudgets(); break;
      case 'tasks': loadTasks(); break;
      case 'workflows': loadWorkflows(); break;
      case 'guardrails': loadGuardrails(); break;
      case 'webhooks': loadWebhooks(); break;
      case 'dlq': loadDlq(); break;
      case 'chat': loadChat(); break;
      case 'formulas': loadFormulas(); break;
      case 'users': loadUsers(); break;
      case 'services': loadServices(); break;
      case 'telegram': loadTelegram(); break;
    }
  }

  // -----------------------------------------------------------------------
  // Exports
  // -----------------------------------------------------------------------
  window.Ctrl = {
    toggleAgent: toggleAgent,
    setBudget: setBudget,
    cancelTask: cancelTask,
    retryDlq: retryDlq,
    addWebhook: addWebhook,
    toggleWebhook: toggleWebhook,
    deleteWebhook: deleteWebhook,
    closeChat: closeChat,
    loadFormulas: loadFormulas,
    toggleFormula: toggleFormula,
    viewFormula: viewFormula,
    createFormula: createFormula,
    loadUsers: loadUsers,
    createUser: createUser,
    changeRole: changeRole,
    toggleUser: toggleUser,
    resetPassword: resetPassword,
    registerBot: registerBot,
    disconnectBot: disconnectBot
  };

  // Initial load
  enforceRoles();
  loadMetrics();
  loadAgents();
  setInterval(function () {
    loadMetrics();
    var active = document.querySelector('.tab.active');
    if (active) loadTab(active.dataset.tab);
  }, BL.REFRESH_INTERVAL || 10000);
})();
