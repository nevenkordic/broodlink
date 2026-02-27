/*
 * Broodlink - Multi-agent AI orchestration system
 * Copyright (C) 2025-2026 Neven Kordic <neven@broodlink.ai>
 * SPDX-License-Identifier: AGPL-3.0-or-later
 */

/* Verification Analytics — v0.11.0 */
(function () {
  'use strict';
  var BL = window.Broodlink;
  var dailyChart, confChart;
  var daysSelect = document.getElementById('verify-days');

  // Chart.js global dark-theme defaults
  Chart.defaults.color = '#94a3b8';
  Chart.defaults.borderColor = 'rgba(30, 58, 95, 0.5)';
  Chart.defaults.font.family = "'Inter', system-ui, sans-serif";
  Chart.defaults.font.size = 12;

  function getDays() {
    return daysSelect ? parseInt(daysSelect.value, 10) || 30 : 30;
  }

  function load() {
    var days = getDays();
    Promise.all([
      BL.fetchApi('/api/v1/verification/analytics?days=' + days),
      BL.fetchApi('/api/v1/verification/confidence')
    ]).then(function (res) {
      var analytics = res[0].analytics || [];
      var histogram = res[1].histogram || [];
      renderMetrics(analytics);
      renderDailyChart(analytics);
      renderTable(analytics);
      renderConfidenceChart(histogram);
    }).catch(function () {
      document.getElementById('verify-table-body').innerHTML =
        '<tr><td colspan="7" style="text-align:center;color:var(--text-secondary);">Failed to load verification data.</td></tr>';
    });
  }

  function renderMetrics(analytics) {
    var totals = { total: 0, verified: 0, corrected: 0, timeout: 0, error: 0 };
    analytics.forEach(function (d) {
      totals.total += d.total || 0;
      totals.verified += d.verified || 0;
      totals.corrected += d.corrected || 0;
      totals.timeout += d.timeout || 0;
      totals.error += d.error || 0;
    });

    document.getElementById('verify-total').textContent = totals.total.toLocaleString();
    document.getElementById('verify-verified').textContent = totals.verified.toLocaleString();
    document.getElementById('verify-corrected').textContent = totals.corrected.toLocaleString();
    document.getElementById('verify-timeout').textContent = totals.timeout.toLocaleString();

    var rate = totals.total > 0
      ? Math.round((totals.verified / totals.total) * 100) + '%'
      : '--';
    document.getElementById('verify-rate').textContent = rate;
  }

  function renderDailyChart(analytics) {
    var ctx = document.getElementById('verify-daily-chart');
    var emptyEl = document.getElementById('daily-empty');
    if (!ctx) return;
    if (dailyChart) dailyChart.destroy();

    if (!analytics.length) {
      ctx.parentElement.style.display = 'none';
      if (emptyEl) emptyEl.style.display = 'flex';
      return;
    }
    ctx.parentElement.style.display = 'block';
    if (emptyEl) emptyEl.style.display = 'none';

    var labels = analytics.map(function (d) { return d.date; });

    dailyChart = new Chart(ctx, {
      type: 'bar',
      data: {
        labels: labels,
        datasets: [
          {
            label: 'Verified',
            data: analytics.map(function (d) { return d.verified || 0; }),
            backgroundColor: 'rgba(34, 197, 94, 0.8)',
            hoverBackgroundColor: '#22c55e',
            borderRadius: 2
          },
          {
            label: 'Corrected',
            data: analytics.map(function (d) { return d.corrected || 0; }),
            backgroundColor: 'rgba(234, 179, 8, 0.8)',
            hoverBackgroundColor: '#eab308',
            borderRadius: 2
          },
          {
            label: 'Timeout',
            data: analytics.map(function (d) { return d.timeout || 0; }),
            backgroundColor: 'rgba(239, 68, 68, 0.8)',
            hoverBackgroundColor: '#ef4444',
            borderRadius: 2
          },
          {
            label: 'Error',
            data: analytics.map(function (d) { return d.error || 0; }),
            backgroundColor: 'rgba(107, 114, 128, 0.8)',
            hoverBackgroundColor: '#6b7280',
            borderRadius: 2
          }
        ]
      },
      options: {
        responsive: true,
        maintainAspectRatio: false,
        interaction: { mode: 'index', intersect: false },
        scales: {
          x: {
            stacked: true,
            grid: { display: false },
            ticks: { maxRotation: 45, font: { size: 11 } }
          },
          y: {
            stacked: true,
            beginAtZero: true,
            grid: { color: 'rgba(30, 58, 95, 0.3)' },
            ticks: { stepSize: 1 }
          }
        },
        plugins: {
          legend: {
            position: 'bottom',
            labels: { padding: 16, usePointStyle: true, pointStyleWidth: 10 }
          },
          tooltip: {
            backgroundColor: '#1e293b',
            titleColor: '#e2e8f0',
            bodyColor: '#94a3b8',
            borderColor: '#1e3a5f',
            borderWidth: 1,
            cornerRadius: 8,
            padding: 10
          }
        }
      }
    });
  }

  function renderConfidenceChart(histogram) {
    var ctx = document.getElementById('verify-confidence-chart');
    var emptyEl = document.getElementById('conf-empty');
    if (!ctx) return;
    if (confChart) confChart.destroy();

    var labels = ['1/5', '2/5', '3/5', '4/5', '5/5'];
    var counts = [0, 0, 0, 0, 0];
    var hasData = false;
    histogram.forEach(function (h) {
      if (h.confidence >= 1 && h.confidence <= 5) {
        counts[h.confidence - 1] = h.count;
        hasData = true;
      }
    });

    if (!hasData) {
      ctx.parentElement.style.display = 'none';
      if (emptyEl) emptyEl.style.display = 'flex';
      return;
    }
    ctx.parentElement.style.display = 'block';
    if (emptyEl) emptyEl.style.display = 'none';

    var barColors = [
      'rgba(239, 68, 68, 0.8)',
      'rgba(249, 115, 22, 0.8)',
      'rgba(234, 179, 8, 0.8)',
      'rgba(34, 197, 94, 0.8)',
      'rgba(22, 163, 74, 0.8)'
    ];

    confChart = new Chart(ctx, {
      type: 'bar',
      data: {
        labels: labels,
        datasets: [{
          label: 'Responses',
          data: counts,
          backgroundColor: barColors,
          borderRadius: 4,
          maxBarThickness: 48
        }]
      },
      options: {
        responsive: true,
        maintainAspectRatio: false,
        scales: {
          x: {
            grid: { display: false },
            title: { display: true, text: 'Confidence Score', color: '#94a3b8', font: { size: 11 } }
          },
          y: {
            beginAtZero: true,
            grid: { color: 'rgba(30, 58, 95, 0.3)' },
            ticks: { stepSize: 1 }
          }
        },
        plugins: {
          legend: { display: false },
          tooltip: {
            backgroundColor: '#1e293b',
            titleColor: '#e2e8f0',
            bodyColor: '#94a3b8',
            borderColor: '#1e3a5f',
            borderWidth: 1,
            cornerRadius: 8,
            padding: 10
          }
        }
      }
    });
  }

  function renderTable(analytics) {
    var tbody = document.getElementById('verify-table-body');
    if (!tbody) return;
    if (!analytics.length) {
      tbody.innerHTML = '<tr><td colspan="7" style="text-align:center;color:var(--text-secondary);padding:2rem;">No verification data in this period.</td></tr>';
      return;
    }

    // Most recent first
    var rows = analytics.slice().reverse();
    tbody.innerHTML = rows.map(function (d) {
      var total = d.total || 0;
      var verified = d.verified || 0;
      var accuracy = total > 0 ? Math.round((verified / total) * 100) + '%' : '--';
      var accColor = total === 0 ? '' : verified / total >= 0.8 ? 'color:var(--green)' : verified / total >= 0.5 ? 'color:var(--amber)' : 'color:var(--red)';
      return '<tr>' +
        '<td style="font-family:var(--font-mono);font-size:0.8125rem;">' + BL.escapeHtml(d.date) + '</td>' +
        '<td style="font-weight:600;">' + total + '</td>' +
        '<td style="color:var(--green);">' + verified + '</td>' +
        '<td style="color:var(--amber);">' + (d.corrected || 0) + '</td>' +
        '<td style="color:var(--red);">' + (d.timeout || 0) + '</td>' +
        '<td>' + (d.error || 0) + '</td>' +
        '<td style="font-weight:600;' + accColor + '">' + accuracy + '</td>' +
        '</tr>';
    }).join('');
  }

  if (daysSelect) {
    daysSelect.addEventListener('change', load);
  }

  load();
  setInterval(load, 30000);
})();
