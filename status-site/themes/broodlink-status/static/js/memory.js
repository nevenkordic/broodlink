// Broodlink — Multi-agent AI orchestration
// Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
// SPDX-License-Identifier: AGPL-3.0-or-later

/**
 * Memory page controller.
 *
 * Fetches memory statistics from the status-api and renders summary metrics
 * plus a topic-distribution bar chart (via charts.js).
 *
 * Expected DOM elements (provided by the Hugo layout):
 *   #memory-total      — total memory entry count
 *   #memory-updated    — last-updated timestamp
 *   #memory-topics     — container for the topic distribution chart
 *   #memory-stats      — container for additional statistics
 *
 * Depends on: /js/utils.js   (window.Broodlink)
 *             /js/charts.js  (window.BroodlinkCharts)
 */
(function (window, document) {
  'use strict';

  var B = window.Broodlink;
  if (!B) {
    console.error('[memory] Broodlink utils not loaded.');
    return;
  }

  var fetchApi           = B.fetchApi;
  var formatRelativeTime = B.formatRelativeTime;
  var escapeHtml         = B.escapeHtml;

  // ---------------------------------------------------------------------------
  // Rendering
  // ---------------------------------------------------------------------------

  function renderStats(stats) {
    var totalEl   = document.getElementById('memory-total');
    var updatedEl = document.getElementById('memory-updated');
    var statsEl   = document.getElementById('memory-stats');

    // Total count
    if (totalEl) {
      totalEl.textContent = stats.total_count != null ? String(stats.total_count) : '—';
    }

    // Last updated
    if (updatedEl) {
      updatedEl.textContent = stats.last_updated || stats.max_updated_at
        ? formatRelativeTime(stats.last_updated || stats.max_updated_at)
        : '—';
    }

    // Additional statistics in a definition list for accessibility
    if (statsEl) {
      var items = [];

      if (stats.total_count != null) {
        items.push({ label: 'Total Entries', value: String(stats.total_count) });
      }
      if (stats.agents_count != null) {
        items.push({ label: 'Contributing Agents', value: String(stats.agents_count) });
      }
      if (stats.topics_count != null) {
        items.push({ label: 'Unique Topics', value: String(stats.topics_count) });
      }
      if (stats.last_updated || stats.max_updated_at) {
        items.push({ label: 'Last Updated', value: escapeHtml(formatRelativeTime(stats.last_updated || stats.max_updated_at)) });
      }
      if (stats.avg_entry_length != null) {
        items.push({ label: 'Avg. Entry Length', value: String(stats.avg_entry_length) + ' chars' });
      }

      if (items.length > 0) {
        var html = '<dl class="memory-stats-grid" style="' +
          'display:grid;grid-template-columns:repeat(auto-fill,minmax(12rem,1fr));gap:1rem;">';
        for (var i = 0; i < items.length; i++) {
          html +=
            '<div class="card" style="text-align:center;">' +
              '<dt style="font-size:0.75rem;text-transform:uppercase;letter-spacing:0.05em;' +
                'color:var(--text-secondary);margin-bottom:0.25rem;">' +
                escapeHtml(items[i].label) +
              '</dt>' +
              '<dd style="font-size:1.5rem;font-weight:700;font-family:var(--font-mono);">' +
                items[i].value +
              '</dd>' +
            '</div>';
        }
        html += '</dl>';
        statsEl.innerHTML = html;
      }
    }
  }

  /**
   * Render the topic distribution chart using the CSS bar chart utility
   * from charts.js.
   */
  function renderTopics(stats) {
    var container = document.getElementById('memory-topics');
    if (!container) return;

    var topics = stats.top_topics || stats.topics || [];

    if (!Array.isArray(topics) || topics.length === 0) {
      container.innerHTML =
        '<p style="color:var(--text-secondary);">No topic distribution data available.</p>';
      return;
    }

    // Normalise to { label, value } pairs.
    // If entries have a count field use it; otherwise use content length as a proxy.
    var data = topics.map(function (t) {
      var v = t.count || t.value || (t.content ? t.content.length : 1);
      return {
        label: t.topic || t.name || t.label || 'unknown',
        value: v
      };
    });

    var Charts = window.BroodlinkCharts;
    if (Charts && Charts.renderBarChart) {
      Charts.renderBarChart(container, data, {
        title: 'Top Topics',
        barColor: 'var(--blue)',
        maxBars: 15
      });
    } else {
      // Fallback: simple list if charts.js has not loaded
      var html = '<h4 style="margin-bottom:0.5rem;">Top Topics</h4><ul style="list-style:none;">';
      for (var i = 0; i < Math.min(data.length, 15); i++) {
        html +=
          '<li style="padding:0.25rem 0;">' +
            escapeHtml(data[i].label) + ': ' +
            '<strong>' + escapeHtml(String(data[i].value)) + '</strong>' +
          '</li>';
      }
      html += '</ul>';
      container.innerHTML = html;
    }
  }

  // ---------------------------------------------------------------------------
  // Fetch & init
  // ---------------------------------------------------------------------------

  function loadMemoryStats() {
    fetchApi('/api/v1/memory/stats')
      .then(function (data) {
        var stats = data || {};
        renderStats(stats);
        renderTopics(stats);
      })
      .catch(function (err) {
        console.warn('[memory] Fetch failed:', err.message || err);
        var statsEl = document.getElementById('memory-stats');
        if (statsEl) {
          statsEl.innerHTML =
            '<p style="color:var(--red);">Failed to load memory statistics. ' +
            'The status API may be unreachable.</p>';
        }
      });
  }

  document.addEventListener('DOMContentLoaded', function () {
    loadMemoryStats();
    setInterval(loadMemoryStats, B.REFRESH_INTERVAL);
  });

})(window, document);
