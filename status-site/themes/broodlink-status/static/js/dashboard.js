// Broodlink — Multi-agent AI orchestration
// Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
// SPDX-License-Identifier: AGPL-3.0-or-later

/**
 * Dashboard controller — powers the main index page.
 *
 * Fetches data from the status-api on load and on a configurable interval,
 * then updates DOM elements that are already present in the Hugo template.
 *
 * Depends on: /js/utils.js  (window.Broodlink)
 */
(function (window, document) {
  'use strict';

  var B = window.Broodlink;
  if (!B) {
    console.error('[dashboard] Broodlink utils not loaded.');
    return;
  }

  var fetchApi          = B.fetchApi;
  var formatRelativeTime = B.formatRelativeTime;
  var statusDot         = B.statusDot;
  var escapeHtml        = B.escapeHtml;

  // ---------------------------------------------------------------------------
  // DOM references
  // ---------------------------------------------------------------------------
  var elMetricAgents    = document.getElementById('metric-agents');
  var elMetricTasks     = document.getElementById('metric-tasks');
  var elMetricDecisions = document.getElementById('metric-decisions');
  var elMetricMemories  = document.getElementById('metric-memories');
  var elHealthGrid      = document.getElementById('health-grid');
  var elAgentRoster     = document.getElementById('agent-roster');
  var elActivityFeed    = document.getElementById('activity-feed');
  var elTasksTbody      = document.getElementById('tasks-tbody');
  var elStaleBanner     = document.getElementById('stale-banner');

  // ---------------------------------------------------------------------------
  // State
  // ---------------------------------------------------------------------------
  var failCount = 0;

  // ---------------------------------------------------------------------------
  // Render helpers
  // ---------------------------------------------------------------------------

  /**
   * Set metric value, with a subtle update animation class.
   */
  function setMetric(el, value) {
    if (!el) return;
    var display = (value !== null && value !== undefined) ? String(value) : '—';
    if (el.textContent !== display) {
      el.textContent = display;
    }
  }

  /**
   * Build the health grid cells.
   * Each service object is expected to have: { name, status, latency_ms }
   */
  function renderHealth(services) {
    if (!elHealthGrid || !Array.isArray(services)) return;

    var html = '';
    for (var i = 0; i < services.length; i++) {
      var svc = services[i];
      var name = escapeHtml(svc.name || 'unknown');
      var status = String(svc.status || 'unknown').toLowerCase();
      var latency = svc.latency_ms != null ? escapeHtml(String(svc.latency_ms)) + 'ms' : '—';

      html +=
        '<div class="health-cell">' +
          '<div class="service-name">' + name + '</div>' +
          statusDot(status) + ' ' + escapeHtml(status) +
          '<div class="mono" style="font-size:0.75rem;color:var(--text-secondary);margin-top:0.25rem;">' +
            latency +
          '</div>' +
        '</div>';
    }
    elHealthGrid.innerHTML = html;
  }

  /**
   * Build agent roster cards.
   * Each agent object: { name, status, role, last_seen }
   */
  function renderAgents(agents) {
    if (!elAgentRoster || !Array.isArray(agents)) return;

    if (agents.length === 0) {
      elAgentRoster.innerHTML =
        '<p style="color:var(--text-secondary);">No agents registered.</p>';
      return;
    }

    var html = '';
    for (var i = 0; i < agents.length; i++) {
      var a = agents[i];
      html +=
        '<div class="card">' +
          '<h3>' + escapeHtml(a.display_name || a.name || a.agent_id || 'unnamed') + '</h3>' +
          statusDot(a.status || (a.active ? 'ok' : 'offline')) + ' ' +
          escapeHtml(a.active ? 'active' : 'offline') +
          (a.role
            ? '<div style="font-size:0.8125rem;color:var(--text-secondary);margin-top:0.375rem;">' +
              escapeHtml(a.role) + '</div>'
            : '') +
          (a.last_seen
            ? '<div style="font-size:0.75rem;color:var(--text-secondary);margin-top:0.25rem;">Seen ' +
              escapeHtml(formatRelativeTime(a.last_seen)) + '</div>'
            : '') +
        '</div>';
    }
    elAgentRoster.innerHTML = html;
  }

  /**
   * Build the activity feed list items.
   * Each item: { action, details, agent_name, created_at }
   */
  function renderActivity(items) {
    if (!elActivityFeed || !Array.isArray(items)) return;

    if (items.length === 0) {
      elActivityFeed.innerHTML =
        '<li style="color:var(--text-secondary);">No recent activity.</li>';
      return;
    }

    var limit = Math.min(items.length, 20);
    var html = '';
    for (var i = 0; i < limit; i++) {
      var item = items[i];
      var agent = escapeHtml(item.agent_name || item.agent_id || '');
      var action = escapeHtml(item.action || '');
      var details = escapeHtml(item.details || '');
      var time = formatRelativeTime(item.created_at);

      html +=
        '<li>' +
          '<span class="mono" style="font-weight:600;">' + agent + '</span>' +
          '<span>' + action + (details ? ' — ' + details : '') + '</span>' +
          '<span class="time">' + escapeHtml(time) + '</span>' +
        '</li>';
    }
    elActivityFeed.innerHTML = html;
  }

  // ---------------------------------------------------------------------------
  // Stream progress tracking
  // ---------------------------------------------------------------------------
  var activeStreams = {}; // stream_id → EventSource

  function connectStream(streamId, barEl) {
    if (activeStreams[streamId]) return; // already connected

    var apiKey = B.API_KEY || '';
    var url = '/api/v1/stream/' + encodeURIComponent(streamId) + '?key=' + encodeURIComponent(apiKey);
    var es = new EventSource(url);
    activeStreams[streamId] = es;

    es.addEventListener('progress', function (e) {
      if (barEl) {
        var data = {};
        try { data = JSON.parse(e.data); } catch (_) {}
        var step = data.step || 'working';
        barEl.setAttribute('title', step);
        barEl.querySelector('.stream-bar-fill').style.width = '60%';
      }
    });

    es.addEventListener('complete', function () {
      if (barEl) {
        barEl.querySelector('.stream-bar-fill').style.width = '100%';
        barEl.querySelector('.stream-bar-fill').style.background = 'var(--success, #4ade80)';
      }
      es.close();
      delete activeStreams[streamId];
    });

    es.addEventListener('error', function () {
      if (barEl) {
        barEl.querySelector('.stream-bar-fill').style.background = 'var(--error, #f87171)';
      }
      es.close();
      delete activeStreams[streamId];
    });

    es.onerror = function () {
      es.close();
      delete activeStreams[streamId];
    };
  }

  /**
   * Build task table rows.
   * Each task: { id, task_type, status, agent_name, created_at, stream_id }
   */
  function renderTasks(tasks) {
    if (!elTasksTbody || !Array.isArray(tasks)) return;

    if (tasks.length === 0) {
      elTasksTbody.innerHTML =
        '<tr><td colspan="5" style="color:var(--text-secondary);">No tasks in queue.</td></tr>';
      return;
    }

    var limit = Math.min(tasks.length, 10);
    var html = '';
    for (var i = 0; i < limit; i++) {
      var t = tasks[i];
      var streamBar = '';
      if (t.stream_id && (t.status === 'claimed' || t.status === 'in_progress')) {
        streamBar =
          '<div class="stream-bar" data-stream-id="' + escapeHtml(t.stream_id) + '">' +
            '<div class="stream-bar-fill" style="width:30%"></div>' +
          '</div>';
      }
      html +=
        '<tr>' +
          '<td class="mono">' + escapeHtml(String(t.id || '—')) + '</td>' +
          '<td>' + escapeHtml(t.title || t.task_type || t.type || '—') + streamBar + '</td>' +
          '<td>' + statusDot(t.status) + ' ' + escapeHtml(t.status || '—') + '</td>' +
          '<td>' + escapeHtml(t.assigned_agent || t.agent_id || t.agent_name || '—') + '</td>' +
          '<td class="mono">' + escapeHtml(formatRelativeTime(t.created_at)) + '</td>' +
        '</tr>';
    }
    elTasksTbody.innerHTML = html;

    // Connect SSE for tasks with active streams
    var bars = elTasksTbody.querySelectorAll('.stream-bar[data-stream-id]');
    for (var j = 0; j < bars.length; j++) {
      var bar = bars[j];
      var sid = bar.getAttribute('data-stream-id');
      if (sid) connectStream(sid, bar);
    }
  }

  // ---------------------------------------------------------------------------
  // Data fetching
  // ---------------------------------------------------------------------------

  function showStaleBanner(visible) {
    if (!elStaleBanner) return;
    if (visible) {
      elStaleBanner.classList.add('visible');
    } else {
      elStaleBanner.classList.remove('visible');
    }
  }

  /**
   * Run all dashboard fetches in parallel, update the DOM, and handle
   * connectivity issues.
   */
  function refresh() {
    var hadError = false;

    function onError(label) {
      return function (err) {
        console.warn('[dashboard] Failed to fetch ' + label + ':', err.message || err);
        hadError = true;
        return null;
      };
    }

    var pAgents   = fetchApi('/api/v1/agents').catch(onError('agents'));
    var pTasks    = fetchApi('/api/v1/tasks').catch(onError('tasks'));
    var pSummary  = fetchApi('/api/v1/summary').catch(onError('summary'));
    var pHealth   = fetchApi('/api/v1/health').catch(onError('health'));
    var pActivity = fetchApi('/api/v1/activity').catch(onError('activity'));
    var pMemory   = fetchApi('/api/v1/memory/stats').catch(onError('memory'));

    Promise.all([pAgents, pTasks, pSummary, pHealth, pActivity, pMemory])
      .then(function (results) {
        var agents   = results[0];
        var tasks    = results[1];
        var summary  = results[2];
        var health   = results[3];
        var activity = results[4];
        var memory   = results[5];

        // -- Metrics ---
        if (agents !== null) {
          var agentList = Array.isArray(agents) ? agents : (agents.agents || []);
          setMetric(elMetricAgents, agentList.length);
          renderAgents(agentList);
        }

        if (tasks !== null) {
          var taskList = Array.isArray(tasks) ? tasks : (tasks.recent || tasks.tasks || []);
          // Use server-provided count when available (recent list may not include all pending)
          var pendingCount = (tasks.counts_by_status && tasks.counts_by_status.pending != null)
            ? tasks.counts_by_status.pending
            : taskList.filter(function (t) { return t.status === 'pending' || t.status === 'queued'; }).length;
          setMetric(elMetricTasks, pendingCount);
          renderTasks(taskList);
        }

        if (summary !== null) {
          // API returns {summaries: [{decisions_made, memories_stored, ...}]}
          var s = summary;
          if (summary.summaries && summary.summaries.length > 0) {
            s = summary.summaries[0];
          }
          setMetric(elMetricDecisions, s.decisions_made != null ? s.decisions_made : (s.decisions_today != null ? s.decisions_today : '—'));
        }

        if (memory !== null) {
          // API returns {total_count, top_topics, ...}
          setMetric(elMetricMemories, memory.total_count != null ? memory.total_count : '—');
        }

        if (health !== null) {
          // API returns {dependencies: {name: {status, latency_ms}}, ...}
          var services = [];
          if (health.dependencies) {
            var deps = health.dependencies;
            var keys = Object.keys(deps);
            for (var k = 0; k < keys.length; k++) {
              var dep = deps[keys[k]];
              services.push({ name: keys[k], status: dep.status, latency_ms: dep.latency_ms });
            }
          } else if (Array.isArray(health)) {
            services = health;
          } else if (health.services) {
            services = health.services;
          }
          renderHealth(services);
        }

        if (activity !== null) {
          var activityList = Array.isArray(activity) ? activity : (activity.activity || activity.items || []);
          renderActivity(activityList);
        }

        // -- Stale banner ---
        if (hadError) {
          failCount++;
        } else {
          failCount = 0;
        }
        // Show banner after 2 consecutive refresh cycles with at least one error
        showStaleBanner(failCount >= 2);
      });
  }

  // ---------------------------------------------------------------------------
  // Initialise
  // ---------------------------------------------------------------------------
  document.addEventListener('DOMContentLoaded', function () {
    refresh();
    setInterval(refresh, B.REFRESH_INTERVAL);
  });

})(window, document);
