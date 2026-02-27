/* Verification Analytics — v0.11.0 */
(function () {
  'use strict';
  var BL = window.Broodlink;
  var dailyChart, confChart;

  function load() {
    Promise.all([
      BL.fetchApi('/api/v1/verification/analytics?days=30'),
      BL.fetchApi('/api/v1/verification/confidence')
    ]).then(function (res) {
      renderMetrics(res[0].analytics || []);
      renderDailyChart(res[0].analytics || []);
      renderTable(res[0].analytics || []);
      renderConfidenceChart(res[1].histogram || []);
    }).catch(function () {
      document.getElementById('verify-table-body').innerHTML =
        '<tr><td colspan="7">Failed to load verification data.</td></tr>';
    });
  }

  function renderMetrics(analytics) {
    var totals = { total: 0, verified: 0, corrected: 0, timeout: 0 };
    analytics.forEach(function (d) {
      totals.total += d.total || 0;
      totals.verified += d.verified || 0;
      totals.corrected += d.corrected || 0;
      totals.timeout += d.timeout || 0;
    });
    document.getElementById('verify-total').textContent = totals.total;
    document.getElementById('verify-verified').textContent = totals.verified;
    document.getElementById('verify-corrected').textContent = totals.corrected;
    document.getElementById('verify-timeout').textContent = totals.timeout;
  }

  function renderDailyChart(analytics) {
    var ctx = document.getElementById('verify-daily-chart');
    if (!ctx) return;
    if (dailyChart) dailyChart.destroy();

    var labels = analytics.map(function (d) { return d.date; });
    dailyChart = new Chart(ctx, {
      type: 'bar',
      data: {
        labels: labels,
        datasets: [
          { label: 'Verified', data: analytics.map(function (d) { return d.verified || 0; }), backgroundColor: '#22c55e' },
          { label: 'Corrected', data: analytics.map(function (d) { return d.corrected || 0; }), backgroundColor: '#eab308' },
          { label: 'Timeout', data: analytics.map(function (d) { return d.timeout || 0; }), backgroundColor: '#ef4444' },
          { label: 'Error', data: analytics.map(function (d) { return d.error || 0; }), backgroundColor: '#6b7280' }
        ]
      },
      options: {
        responsive: true,
        scales: { x: { stacked: true }, y: { stacked: true, beginAtZero: true } },
        plugins: { legend: { position: 'bottom' } }
      }
    });
  }

  function renderConfidenceChart(histogram) {
    var ctx = document.getElementById('verify-confidence-chart');
    if (!ctx) return;
    if (confChart) confChart.destroy();

    var labels = ['1', '2', '3', '4', '5'];
    var counts = [0, 0, 0, 0, 0];
    histogram.forEach(function (h) {
      if (h.confidence >= 1 && h.confidence <= 5) {
        counts[h.confidence - 1] = h.count;
      }
    });

    confChart = new Chart(ctx, {
      type: 'bar',
      data: {
        labels: labels,
        datasets: [{
          label: 'Count',
          data: counts,
          backgroundColor: ['#ef4444', '#f97316', '#eab308', '#22c55e', '#16a34a']
        }]
      },
      options: {
        responsive: true,
        scales: { y: { beginAtZero: true } },
        plugins: { legend: { display: false } }
      }
    });
  }

  function renderTable(analytics) {
    var tbody = document.getElementById('verify-table-body');
    if (!tbody) return;
    if (!analytics.length) {
      tbody.innerHTML = '<tr><td colspan="7">No verification data yet.</td></tr>';
      return;
    }
    tbody.innerHTML = analytics.reverse().map(function (d) {
      return '<tr>' +
        '<td>' + d.date + '</td>' +
        '<td>' + (d.total || 0) + '</td>' +
        '<td>' + (d.verified || 0) + '</td>' +
        '<td>' + (d.corrected || 0) + '</td>' +
        '<td>' + (d.timeout || 0) + '</td>' +
        '<td>' + (d.error || 0) + '</td>' +
        '<td>' + (d.avg_duration_ms || '--') + '</td>' +
        '</tr>';
    }).join('');
  }

  load();
  setInterval(load, 30000);
})();
