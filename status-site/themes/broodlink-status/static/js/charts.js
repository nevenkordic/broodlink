// Broodlink — Multi-agent AI orchestration
// Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
// SPDX-License-Identifier: AGPL-3.0-or-later

/**
 * Lightweight CSS bar-chart utilities.
 *
 * No external dependencies (no Chart.js, no D3). Charts are rendered as
 * semantic HTML with ARIA attributes and CSS-driven bars.
 *
 * WCAG 2.1 AA: every bar carries a visible text value so information is not
 * conveyed by size or colour alone. The chart container uses role="img" with
 * an aria-label summary.
 *
 * Depends on: /js/utils.js  (window.Broodlink — for escapeHtml only)
 */
(function (window, document) {
  'use strict';

  var B = window.Broodlink;
  var escapeHtml = (B && B.escapeHtml) ? B.escapeHtml : function (s) { return String(s); };

  // ---------------------------------------------------------------------------
  // renderBarChart
  // ---------------------------------------------------------------------------
  /**
   * Render a horizontal bar chart using pure CSS inside `container`.
   *
   * @param {HTMLElement}  container   - DOM element to render into (innerHTML
   *                                     will be replaced).
   * @param {Array<{label:string, value:number}>}  data  - chart data
   * @param {Object}       [options]
   * @param {string}       [options.title]     - optional heading above chart
   * @param {string}       [options.barColor]  - CSS colour for bars
   *                                             (default: var(--blue))
   * @param {number}       [options.maxBars]   - max bars to render
   *                                             (default: 20)
   * @param {string}       [options.unit]      - unit label after values
   *                                             (default: "")
   */
  function renderBarChart(container, data, options) {
    if (!container || !Array.isArray(data) || data.length === 0) {
      if (container) {
        container.innerHTML =
          '<p style="color:var(--text-secondary);">No data available for chart.</p>';
      }
      return;
    }

    var opts = options || {};
    var barColor = opts.barColor || 'var(--blue)';
    var maxBars  = opts.maxBars  || 20;
    var unit     = opts.unit     || '';
    var title    = opts.title    || '';

    // Sort descending by value and clamp
    var sorted = data.slice().sort(function (a, b) { return b.value - a.value; });
    sorted = sorted.slice(0, maxBars);

    // Determine maximum value for scaling
    var maxVal = 0;
    for (var i = 0; i < sorted.length; i++) {
      if (sorted[i].value > maxVal) maxVal = sorted[i].value;
    }
    if (maxVal === 0) maxVal = 1; // prevent division by zero

    // Build accessible summary
    var summary = (title ? title + ': ' : '') + sorted.length + ' items, ' +
      'max value ' + maxVal + (unit ? ' ' + unit : '');

    // Build HTML
    var html = '';

    if (title) {
      html += '<h4 style="margin-bottom:0.75rem;">' + escapeHtml(title) + '</h4>';
    }

    html += '<div role="img" aria-label="' + escapeHtml(summary) + '" ' +
      'style="display:flex;flex-direction:column;gap:0.375rem;">';

    for (var j = 0; j < sorted.length; j++) {
      var item = sorted[j];
      var pct  = Math.round((item.value / maxVal) * 100);
      // Ensure at least 2% width so very small values are still visible
      var barWidth = Math.max(pct, 2);

      html +=
        '<div style="display:grid;grid-template-columns:10rem 1fr 3rem;gap:0.5rem;align-items:center;">' +
          // Label
          '<span style="font-size:0.8125rem;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;" ' +
            'title="' + escapeHtml(item.label) + '">' +
            escapeHtml(item.label) +
          '</span>' +
          // Bar
          '<div style="background:var(--bg-tertiary);border-radius:0.25rem;height:1.25rem;overflow:hidden;">' +
            '<div style="' +
              'width:' + barWidth + '%;' +
              'height:100%;' +
              'background:' + barColor + ';' +
              'border-radius:0.25rem;' +
              'transition:width 0.3s ease;' +
            '" aria-hidden="true"></div>' +
          '</div>' +
          // Value
          '<span class="mono" style="font-size:0.75rem;text-align:right;color:var(--text-secondary);">' +
            escapeHtml(String(item.value)) + (unit ? ' ' + escapeHtml(unit) : '') +
          '</span>' +
        '</div>';
    }

    html += '</div>';

    container.innerHTML = html;
  }

  // ---------------------------------------------------------------------------
  // Export
  // ---------------------------------------------------------------------------
  window.BroodlinkCharts = {
    renderBarChart: renderBarChart
  };

})(window, document);
