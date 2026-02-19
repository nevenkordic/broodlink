// Broodlink — Multi-agent AI orchestration
// Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
// SPDX-License-Identifier: AGPL-3.0-or-later

/**
 * Shared utilities for the Broodlink status dashboard.
 * Vanilla JS, no dependencies. WCAG 2.1 AA compliant.
 */
(function (window) {
  'use strict';

  // ---------------------------------------------------------------------------
  // Configuration
  // ---------------------------------------------------------------------------
  // Prefer <meta name="broodlink:statusApiUrl" content="..."> if present,
  // then fall back to a global BroodlinkConfig object, then to sensible
  // defaults.
  // ---------------------------------------------------------------------------

  function metaContent(name) {
    var el = document.querySelector('meta[name="' + name + '"]');
    return el ? el.getAttribute('content') : null;
  }

  var cfg = window.BroodlinkConfig || {};

  var STATUS_API_URL = (
    metaContent('broodlink:statusApiUrl') ||
    cfg.statusApiUrl ||
    'http://localhost:3312'
  ).replace(/\/+$/, ''); // strip trailing slash

  var STATUS_API_KEY = (
    metaContent('broodlink:statusApiKey') ||
    cfg.statusApiKey ||
    ''
  );

  var REFRESH_INTERVAL = parseInt(
    metaContent('broodlink:refreshInterval') ||
    cfg.refreshInterval ||
    '10000',
    10
  );

  // ---------------------------------------------------------------------------
  // fetchApi
  // ---------------------------------------------------------------------------
  /**
   * Fetch JSON from the status API.
   *
   * @param {string} path  - API path, e.g. "/api/v1/agents"
   * @returns {Promise<*>}  - Resolves with the `.data` property of the
   *                          response JSON, or the full body if `.data` is
   *                          absent.
   */
  function fetchApi(path) {
    var url = STATUS_API_URL + path;

    return fetch(url, {
      method: 'GET',
      headers: {
        'Accept': 'application/json',
        'X-Broodlink-Api-Key': STATUS_API_KEY
      },
      signal: AbortSignal.timeout ? AbortSignal.timeout(8000) : undefined
    })
      .then(function (res) {
        if (!res.ok) {
          throw new Error('HTTP ' + res.status + ' from ' + url);
        }
        return res.json();
      })
      .then(function (json) {
        return json.data !== undefined ? json.data : json;
      });
  }

  // ---------------------------------------------------------------------------
  // formatRelativeTime
  // ---------------------------------------------------------------------------
  /**
   * Convert an ISO 8601 timestamp to a human-readable relative string.
   *
   * @param {string} isoString  - e.g. "2026-02-18T12:00:00Z"
   * @returns {string}          - e.g. "2m ago", "1h ago", "3d ago"
   */
  function formatRelativeTime(isoString) {
    if (!isoString) return '—';

    var now = Date.now();
    var then = new Date(isoString).getTime();
    if (isNaN(then)) return '—';

    var diffSec = Math.max(0, Math.floor((now - then) / 1000));

    if (diffSec < 60) return diffSec + 's ago';
    var diffMin = Math.floor(diffSec / 60);
    if (diffMin < 60) return diffMin + 'm ago';
    var diffHr = Math.floor(diffMin / 60);
    if (diffHr < 24) return diffHr + 'h ago';
    var diffDay = Math.floor(diffHr / 24);
    if (diffDay < 30) return diffDay + 'd ago';
    var diffMon = Math.floor(diffDay / 30);
    if (diffMon < 12) return diffMon + 'mo ago';
    return Math.floor(diffMon / 12) + 'y ago';
  }

  // ---------------------------------------------------------------------------
  // statusDot
  // ---------------------------------------------------------------------------
  /**
   * Return an HTML string for a status indicator dot.
   *
   * WCAG note: the dot is accompanied by an aria-label so the status is
   * conveyed via more than colour alone. Downstream markup should include a
   * visible text label as well (e.g. "OK", "degraded").
   *
   * Uses the CSS class `.status-indicator` and the status-specific modifier
   * classes (`.ok`, `.degraded`, `.offline`) defined in style.css, plus an
   * inline SVG circle for the visual dot.
   *
   * @param {string} status  - one of "ok", "degraded", "offline", or any
   *                           other string (falls back to "unknown").
   * @returns {string}        - HTML snippet
   */
  function statusDot(status) {
    var safe = escapeHtml(String(status || 'unknown').toLowerCase());
    // SVG circle provides the visual indicator
    var svg =
      '<svg width="12" height="12" viewBox="0 0 12 12" aria-hidden="true">' +
      '<circle cx="6" cy="6" r="5" fill="currentColor"/>' +
      '</svg>';
    return (
      '<span class="status-indicator ' + safe + '" role="img" aria-label="' + safe + '">' +
      svg + ' ' + safe +
      '</span>'
    );
  }

  // ---------------------------------------------------------------------------
  // escapeHtml
  // ---------------------------------------------------------------------------
  /**
   * Escape HTML special characters to prevent XSS when interpolating
   * user-controlled data into the DOM via innerHTML.
   *
   * @param {string} str
   * @returns {string}
   */
  var ESCAPE_MAP = {
    '&': '&amp;',
    '<': '&lt;',
    '>': '&gt;',
    '"': '&quot;',
    "'": '&#x27;',
    '/': '&#x2F;'
  };

  function escapeHtml(str) {
    if (typeof str !== 'string') return '';
    return str.replace(/[&<>"'/]/g, function (ch) {
      return ESCAPE_MAP[ch];
    });
  }

  // ---------------------------------------------------------------------------
  // debounce
  // ---------------------------------------------------------------------------
  /**
   * Standard debounce. The wrapped function fires only after `ms` milliseconds
   * of inactivity.
   *
   * @param {Function} fn
   * @param {number}   ms
   * @returns {Function}
   */
  function debounce(fn, ms) {
    var timer;
    return function () {
      var ctx = this;
      var args = arguments;
      clearTimeout(timer);
      timer = setTimeout(function () {
        fn.apply(ctx, args);
      }, ms);
    };
  }

  // ---------------------------------------------------------------------------
  // Exports
  // ---------------------------------------------------------------------------
  window.Broodlink = {
    fetchApi: fetchApi,
    formatRelativeTime: formatRelativeTime,
    statusDot: statusDot,
    escapeHtml: escapeHtml,
    debounce: debounce,
    STATUS_API_URL: STATUS_API_URL,
    STATUS_API_KEY: STATUS_API_KEY,
    REFRESH_INTERVAL: REFRESH_INTERVAL
  };

})(window);
