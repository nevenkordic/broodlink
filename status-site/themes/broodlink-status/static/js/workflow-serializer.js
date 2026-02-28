/*
 * Broodlink - Multi-agent AI orchestration system
 * Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
 * SPDX-License-Identifier: AGPL-3.0-or-later
 *
 * Workflow Serializer — bidirectional Formula ↔ graph conversion
 */

(function () {
  'use strict';

  var idCounter = 0;
  function nextId() { return 'n_' + (++idCounter); }

  // ── Formula → Graph ─────────────────────────────────────────────

  /**
   * Convert API formula response to editor graph format.
   * @param {Object} formulaData - from GET /api/v1/formulas/:name
   * @returns {{nodes: Array, edges: Array}}
   */
  function formulaToGraph(formulaData) {
    idCounter = 0;
    var def = formulaData.definition || {};
    var steps = def.steps || [];
    var nodes = [];
    var edges = [];
    var outputToNodeId = {};

    // Start node
    var startId = nextId();
    nodes.push({ _id: startId, type: 'start', name: 'Start', _x: 0, _y: 0 });

    // Step nodes
    steps.forEach(function (step) {
      var nid = nextId();
      nodes.push({
        _id: nid,
        type: step.when ? 'condition' : 'step',
        name: step.name || '',
        agent_role: step.agent_role || 'worker',
        tools: step.tools || [],
        prompt: step.prompt || '',
        output: step.output || '',
        input: step.input || null,
        when: step.when || null,
        retries: step.retries != null ? step.retries : null,
        backoff: step.backoff != null ? step.backoff : null,
        timeout_seconds: step.timeout_seconds != null ? step.timeout_seconds : null,
        group: step.group != null ? step.group : null,
        examples: step.examples || null,
        system_prompt: step.system_prompt || null,
        output_schema: step.output_schema || null,
        model_domain: step.model_domain || null,
        _x: 0,
        _y: 0,
      });
      if (step.output) outputToNodeId[step.output] = nid;
    });

    // End node
    var endId = nextId();
    nodes.push({ _id: endId, type: 'end', name: 'End', _x: 0, _y: 0 });

    // Build edges from input references
    var nodeByName = {};
    nodes.forEach(function (n) {
      if (n.type === 'step' || n.type === 'condition') nodeByName[n.name] = n;
    });

    steps.forEach(function (step) {
      var target = nodeByName[step.name];
      if (!target) return;

      if (step.input) {
        var inputs = typeof step.input === 'string' ? [step.input] : (Array.isArray(step.input) ? step.input : []);
        inputs.forEach(function (inp) {
          var srcId = outputToNodeId[inp];
          if (srcId) {
            edges.push({ from: srcId, to: target._id });
          }
        });
      }
    });

    // Connect steps with no inbound edges to Start
    var hasInbound = {};
    edges.forEach(function (e) { hasInbound[e.to] = true; });
    nodes.forEach(function (n) {
      if ((n.type === 'step' || n.type === 'condition') && !hasInbound[n._id]) {
        edges.push({ from: startId, to: n._id });
      }
    });

    // Connect steps with no outbound edges to End
    var hasOutbound = {};
    edges.forEach(function (e) { hasOutbound[e.from] = true; });
    nodes.forEach(function (n) {
      if ((n.type === 'step' || n.type === 'condition') && !hasOutbound[n._id]) {
        edges.push({ from: n._id, to: endId });
      }
    });

    return { nodes: nodes, edges: edges };
  }

  // ── Graph → Formula ─────────────────────────────────────────────

  /**
   * Convert editor graph back to API formula definition.
   * @param {Object} state - {nodes, edges, definition}
   * @returns {Object} definition for API
   */
  function graphToFormula(state) {
    var stepNodes = state.nodes.filter(function (n) {
      return n.type === 'step' || n.type === 'condition';
    });

    // Build inbound edge map (target → [source IDs])
    var inbound = {};
    state.edges.forEach(function (e) {
      if (!inbound[e.to]) inbound[e.to] = [];
      inbound[e.to].push(e.from);
    });

    // Node ID → output variable
    var nodeOutputs = {};
    stepNodes.forEach(function (n) { nodeOutputs[n._id] = n.output; });

    // Topological sort
    var sorted = topSort(stepNodes, state.edges);

    var steps = sorted.map(function (n) {
      var step = {
        name: n.name,
        agent_role: n.agent_role || 'worker',
        prompt: n.prompt || '',
        output: n.output || '',
      };

      // Tools
      if (n.tools && n.tools.length > 0) step.tools = n.tools;

      // Resolve input from inbound edges (skip Start node sources)
      var sources = (inbound[n._id] || [])
        .map(function (srcId) { return nodeOutputs[srcId]; })
        .filter(Boolean);

      if (sources.length === 1) {
        step.input = sources[0];
      } else if (sources.length > 1) {
        step.input = sources;
      }

      // Optional fields
      if (n.when) step.when = n.when;
      if (n.retries != null) step.retries = n.retries;
      if (n.backoff != null) step.backoff = n.backoff;
      if (n.timeout_seconds != null) step.timeout_seconds = n.timeout_seconds;
      if (n.group != null) step.group = n.group;
      if (n.examples) step.examples = n.examples;
      if (n.system_prompt) step.system_prompt = n.system_prompt;
      if (n.output_schema) step.output_schema = n.output_schema;
      if (n.model_domain) step.model_domain = n.model_domain;

      return step;
    });

    return {
      formula: state.definition ? state.definition.formula : {},
      parameters: state.definition ? (state.definition.parameters || []) : [],
      steps: steps,
      on_failure: state.definition ? (state.definition.on_failure || null) : null,
    };
  }

  // ── Topological Sort (Kahn's) ───────────────────────────────────

  function topSort(nodes, edges) {
    var ids = {};
    nodes.forEach(function (n) { ids[n._id] = true; });

    var inDeg = {};
    var adj = {};
    nodes.forEach(function (n) { inDeg[n._id] = 0; adj[n._id] = []; });

    edges.forEach(function (e) {
      if (ids[e.from] && ids[e.to]) {
        adj[e.from].push(e.to);
        inDeg[e.to] = (inDeg[e.to] || 0) + 1;
      }
    });

    var queue = [];
    nodes.forEach(function (n) {
      if (inDeg[n._id] === 0) queue.push(n._id);
    });

    var sorted = [];
    while (queue.length > 0) {
      var id = queue.shift();
      sorted.push(id);
      (adj[id] || []).forEach(function (nb) {
        inDeg[nb]--;
        if (inDeg[nb] === 0) queue.push(nb);
      });
    }

    return sorted.map(function (id) {
      return nodes.find(function (n) { return n._id === id; });
    }).filter(Boolean);
  }

  // ── Cycle Detection (DFS) ──────────────────────────────────────

  /**
   * Detect cycles in the graph.
   * @param {Array} nodes
   * @param {Array} edges
   * @returns {Array} cycle edges [{from, to}]
   */
  function detectCycles(nodes, edges) {
    var adj = {};
    nodes.forEach(function (n) { adj[n._id] = []; });
    edges.forEach(function (e) {
      if (adj[e.from]) adj[e.from].push(e.to);
    });

    var WHITE = 0, GRAY = 1, BLACK = 2;
    var color = {};
    nodes.forEach(function (n) { color[n._id] = WHITE; });
    var cycleEdges = [];

    function dfs(u) {
      color[u] = GRAY;
      var neighbors = adj[u] || [];
      for (var i = 0; i < neighbors.length; i++) {
        var v = neighbors[i];
        if (color[v] === GRAY) {
          cycleEdges.push({ from: u, to: v });
        } else if (color[v] === WHITE) {
          dfs(v);
        }
      }
      color[u] = BLACK;
    }

    nodes.forEach(function (n) {
      if (color[n._id] === WHITE) dfs(n._id);
    });

    return cycleEdges;
  }

  // ── Exports ─────────────────────────────────────────────────────

  window.WorkflowSerializer = {
    formulaToGraph: formulaToGraph,
    graphToFormula: graphToFormula,
    detectCycles: detectCycles,
  };
})();
