/*
 * Broodlink - Multi-agent AI orchestration system
 * Copyright (C) 2025-2026 Neven Kordic <neven@broodlink.ai>
 * SPDX-License-Identifier: AGPL-3.0-only
 *
 * Knowledge Graph dashboard page
 */
(function () {
  'use strict';

  var elTotalEntities = document.getElementById('kg-total-entities');
  var elActiveEdges   = document.getElementById('kg-active-edges');
  var elHistorical    = document.getElementById('kg-historical-edges');
  var typesChart      = document.getElementById('kg-types-chart');
  var relationsChart  = document.getElementById('kg-relations-chart');
  var connectedTbody  = document.getElementById('kg-connected-tbody');
  var edgesTbody      = document.getElementById('kg-edges-tbody');

  if (!elTotalEntities) return;

  var BL = window.Broodlink;
  var Charts = window.BroodlinkCharts;

  function setMetric(el, value) {
    if (!el) return;
    el.textContent = value != null ? String(value) : '\u2014';
  }

  function renderStats(stats) {
    setMetric(elTotalEntities, stats.total_entities);
    setMetric(elActiveEdges, stats.total_active_edges);
    setMetric(elHistorical, stats.total_historical_edges);

    // Entity type distribution chart
    if (typesChart && Charts && stats.entity_types) {
      var typeData = Object.keys(stats.entity_types).map(function (key) {
        return { label: key, value: stats.entity_types[key] };
      });
      Charts.renderBarChart(typesChart, typeData, {
        title: 'Entity Types',
        barColor: 'var(--green, #22c55e)',
        maxBars: 10
      });
    }

    // Top relationship types chart
    if (relationsChart && Charts && stats.top_relation_types) {
      var relData = stats.top_relation_types.map(function (r) {
        return { label: r.relation, value: r.count };
      });
      Charts.renderBarChart(relationsChart, relData, {
        title: 'Top Relationships',
        barColor: 'var(--blue, #3b82f6)',
        maxBars: 10
      });
    }

    // Most connected entities table
    if (connectedTbody && stats.most_connected_entities) {
      if (stats.most_connected_entities.length === 0) {
        connectedTbody.innerHTML = '<tr><td colspan="3">No entities yet.</td></tr>';
      } else {
        connectedTbody.innerHTML = stats.most_connected_entities.map(function (e) {
          return '<tr>' +
            '<td>' + BL.escapeHtml(e.name) + '</td>' +
            '<td><code>' + BL.escapeHtml(e.type) + '</code></td>' +
            '<td>' + e.edge_count + '</td>' +
            '</tr>';
        }).join('');
      }
    }
  }

  function renderEdges(edges) {
    if (!edgesTbody) return;
    if (!edges || edges.length === 0) {
      edgesTbody.innerHTML = '<tr><td colspan="5">No edges yet.</td></tr>';
      return;
    }

    edgesTbody.innerHTML = edges.map(function (e) {
      return '<tr>' +
        '<td>' + BL.escapeHtml(e.source) + '</td>' +
        '<td><code>' + BL.escapeHtml(e.relation_type) + '</code></td>' +
        '<td>' + BL.escapeHtml(e.target) + '</td>' +
        '<td>' + (e.weight != null ? e.weight.toFixed(1) : '1.0') + '</td>' +
        '<td>' + BL.formatRelativeTime(e.created_at) + '</td>' +
        '</tr>';
    }).join('');
  }

  function load() {
    BL.fetchApi('/api/v1/kg/stats')
      .then(renderStats)
      .catch(function () {
        setMetric(elTotalEntities, '\u2014');
        setMetric(elActiveEdges, '\u2014');
        setMetric(elHistorical, '\u2014');
      });

    BL.fetchApi('/api/v1/kg/edges?limit=20')
      .then(function (data) {
        renderEdges(data.edges || data);
      })
      .catch(function () {
        if (edgesTbody) edgesTbody.innerHTML = '<tr><td colspan="5">Failed to load edges.</td></tr>';
      });
  }

  load();
  setInterval(load, BL.REFRESH_INTERVAL || 10000);
})();
