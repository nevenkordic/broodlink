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
    else if (s === 'pending' || s === 'submitted' || s === 'running') cls += 'ctrl-badge-pending';
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
    return fetch(BL.STATUS_API_URL + path, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'X-Broodlink-Api-Key': BL.STATUS_API_KEY
      },
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
  function loadTab(name) {
    switch (name) {
      case 'agents': loadAgents(); break;
      case 'budgets': loadBudgets(); break;
      case 'tasks': loadTasks(); break;
      case 'workflows': loadWorkflows(); break;
      case 'guardrails': loadGuardrails(); break;
      case 'webhooks': loadWebhooks(); break;
      case 'dlq': loadDlq(); break;
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
    deleteWebhook: deleteWebhook
  };

  // Initial load
  loadMetrics();
  loadAgents();
  setInterval(function () {
    loadMetrics();
    var active = document.querySelector('.tab.active');
    if (active) loadTab(active.dataset.tab);
  }, BL.REFRESH_INTERVAL || 10000);
})();
