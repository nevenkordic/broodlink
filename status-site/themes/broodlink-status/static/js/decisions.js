// Broodlink — Multi-agent AI orchestration
// Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
// SPDX-License-Identifier: AGPL-3.0-or-later

/**
 * Decisions page controller.
 *
 * Fetches decisions from the status-api and renders them as filterable cards.
 *
 * Expected DOM elements (provided by the Hugo layout):
 *   #decisions-search   — text input for filtering
 *   #decisions-list     — container for decision cards
 *   #decisions-count    — element showing the visible/total count
 *
 * Depends on: /js/utils.js  (window.Broodlink)
 */
(function (window, document) {
  'use strict';

  var B = window.Broodlink;
  if (!B) {
    console.error('[decisions] Broodlink utils not loaded.');
    return;
  }

  var fetchApi           = B.fetchApi;
  var formatRelativeTime = B.formatRelativeTime;
  var escapeHtml         = B.escapeHtml;
  var debounce           = B.debounce;

  // ---------------------------------------------------------------------------
  // State
  // ---------------------------------------------------------------------------
  var allDecisions = [];

  // ---------------------------------------------------------------------------
  // Confidence badge
  // ---------------------------------------------------------------------------
  /**
   * Return an HTML badge for a confidence value (0..1 or 0..100).
   * Colour coding is reinforced with a text label so it is not colour-alone
   * (WCAG 2.1 AA).
   */
  function confidenceBadge(value) {
    if (value == null) return '';

    // Normalise to 0..100
    var pct = value <= 1 ? Math.round(value * 100) : Math.round(value);
    var level, cssColor;

    if (pct >= 80) {
      level = 'high';
      cssColor = 'var(--green)';
    } else if (pct >= 50) {
      level = 'medium';
      cssColor = 'var(--amber)';
    } else {
      level = 'low';
      cssColor = 'var(--red)';
    }

    return (
      '<span class="confidence-badge" ' +
        'style="display:inline-block;font-size:0.75rem;font-weight:600;' +
        'padding:0.125rem 0.5rem;border-radius:1rem;border:1px solid ' + cssColor + ';' +
        'color:' + cssColor + ';" ' +
        'aria-label="Confidence: ' + pct + '% (' + level + ')">' +
        pct + '% ' + level +
      '</span>'
    );
  }

  // ---------------------------------------------------------------------------
  // Rendering
  // ---------------------------------------------------------------------------

  /**
   * Truncate a string to `max` characters, adding an ellipsis if trimmed.
   */
  function truncate(str, max) {
    if (!str) return '';
    if (str.length <= max) return str;
    return str.slice(0, max) + '...';
  }

  /**
   * Render a filtered subset of decisions into the container.
   */
  function render(decisions) {
    var container = document.getElementById('decisions-list');
    var countEl   = document.getElementById('decisions-count');

    if (!container) return;

    if (countEl) {
      countEl.textContent = decisions.length + ' of ' + allDecisions.length + ' decisions';
    }

    if (decisions.length === 0) {
      container.innerHTML =
        '<p style="color:var(--text-secondary);">No decisions match the current filter.</p>';
      return;
    }

    var html = '';
    for (var i = 0; i < decisions.length; i++) {
      var d = decisions[i];
      html +=
        '<article class="card" style="margin-bottom:0.75rem;">' +
          '<h3 style="margin-bottom:0.375rem;">' + escapeHtml(d.decision || d.title || 'Untitled') + '</h3>' +
          '<div style="display:flex;gap:0.75rem;align-items:center;flex-wrap:wrap;margin-bottom:0.5rem;">' +
            (d.agent_name || d.agent_id
              ? '<span class="mono" style="font-size:0.8125rem;color:var(--blue);">' +
                escapeHtml(d.agent_name || d.agent_id) + '</span>'
              : '') +
            confidenceBadge(d.confidence) +
            '<span style="font-size:0.75rem;color:var(--text-secondary);">' +
              escapeHtml(formatRelativeTime(d.created_at)) +
            '</span>' +
          '</div>' +
          (d.rationale || d.reasoning
            ? '<p style="font-size:0.875rem;color:var(--text-secondary);line-height:1.5;">' +
              escapeHtml(truncate(d.rationale || d.reasoning, 280)) + '</p>'
            : '') +
        '</article>';
    }
    container.innerHTML = html;
  }

  // ---------------------------------------------------------------------------
  // Filtering
  // ---------------------------------------------------------------------------

  function applyFilter() {
    var searchEl = document.getElementById('decisions-search');
    if (!searchEl) {
      render(allDecisions);
      return;
    }

    var query = searchEl.value.toLowerCase().trim();
    if (!query) {
      render(allDecisions);
      return;
    }

    var filtered = allDecisions.filter(function (d) {
      var haystack = [
        d.decision || d.title || '',
        d.agent_name || d.agent_id || '',
        d.rationale || d.reasoning || ''
      ].join(' ').toLowerCase();
      return haystack.indexOf(query) !== -1;
    });

    render(filtered);
  }

  // ---------------------------------------------------------------------------
  // Fetch & init
  // ---------------------------------------------------------------------------

  function loadDecisions() {
    fetchApi('/api/v1/decisions')
      .then(function (data) {
        allDecisions = Array.isArray(data) ? data : (data.decisions || []);
        applyFilter();
      })
      .catch(function (err) {
        console.warn('[decisions] Fetch failed:', err.message || err);
        var container = document.getElementById('decisions-list');
        if (container) {
          container.innerHTML =
            '<p style="color:var(--red);">Failed to load decisions. The status API may be unreachable.</p>';
        }
      });
  }

  document.addEventListener('DOMContentLoaded', function () {
    var searchEl = document.getElementById('decisions-search');
    if (searchEl) {
      searchEl.addEventListener('input', debounce(applyFilter, 200));
    }

    loadDecisions();
    setInterval(loadDecisions, B.REFRESH_INTERVAL);
  });

})(window, document);
