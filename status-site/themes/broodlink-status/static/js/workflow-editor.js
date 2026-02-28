/*
 * Broodlink - Multi-agent AI orchestration system
 * Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
 * SPDX-License-Identifier: AGPL-3.0-or-later
 *
 * Visual Workflow Editor — standalone page module
 */

(function () {
  'use strict';

  var BL = window.Broodlink;
  var WFS = window.WorkflowSerializer;
  var NS = 'http://www.w3.org/2000/svg';

  // Layout constants
  var NODE_W = 200, NODE_H = 72, MARGIN_X = 60, LAYER_H = 120;
  var PORT_R = 6, GROUP_PAD = 20;
  var START_R = 20, END_R = 16;
  var COND_SIZE = 40; // half-diagonal of diamond
  var GRID_SIZE = 24;
  var MAX_HISTORY = 50;
  var STORAGE_PREFIX = 'broodlink_wfe_pos_';

  var ROLE_COLORS = {
    researcher: { bg: 'rgba(167,139,250,0.18)', fg: '#a78bfa' },
    writer:     { bg: 'rgba(59,130,246,0.18)',   fg: '#3b82f6' },
    architect:  { bg: 'rgba(245,158,11,0.18)',   fg: '#f59e0b' },
    developer:  { bg: 'rgba(34,197,94,0.18)',    fg: '#22c55e' },
    tester:     { bg: 'rgba(239,68,68,0.18)',    fg: '#ef4444' },
    analyst:    { bg: 'rgba(58,110,165,0.18)',    fg: '#3a6ea5' },
    monitor:    { bg: 'rgba(148,163,184,0.12)',   fg: '#94a3b8' },
    worker:     { bg: 'rgba(148,163,184,0.12)',   fg: '#94a3b8' },
  };

  // ── State ────────────────────────────────────────────────────────

  var state = {
    formula: null,
    definition: null,
    nodes: [],
    edges: [],
    selectedId: null,
    selectedEdgeIdx: -1,
    zoom: 1,
    panX: 60,
    panY: 40,
    isDirty: false,
    isReadOnly: false,
    isNew: false,
    history: [],
    historyIndex: -1,
    snapToGrid: false,
    edgeDraw: null,
    dragState: null,
    allTools: [],
    errors: [],
    cycleEdges: [],
  };

  // ── DOM refs ─────────────────────────────────────────────────────

  var svg, canvasGroup, propsPanel, validationBar, titleEl, formulaSelect,
      emptyState, zoomDisplay, contextMenu, canvasContainer;
  var originalFormulaName = null; // preserve original name for save URL

  // ── Init ─────────────────────────────────────────────────────────

  function init() {
    svg = document.getElementById('wfe-svg');
    canvasGroup = document.getElementById('wfe-canvas-group');
    propsPanel = document.getElementById('wfe-props');
    validationBar = document.getElementById('wfe-validation');
    titleEl = document.getElementById('wfe-title');
    formulaSelect = document.getElementById('wfe-formula-select');
    emptyState = document.getElementById('wfe-empty-state');
    zoomDisplay = document.getElementById('wfe-zoom-display');
    contextMenu = document.getElementById('wfe-context-menu');
    canvasContainer = document.getElementById('wfe-canvas-container');

    if (!svg || !canvasGroup) return;

    // Auth check
    if (window.BLAuth) BLAuth.requireAuth();

    // Warn before leaving with unsaved changes
    window.addEventListener('beforeunload', function (ev) {
      if (state.isDirty) {
        ev.preventDefault();
        ev.returnValue = '';
      }
    });

    // Events
    svg.addEventListener('mousedown', onMouseDown);
    svg.addEventListener('mousemove', onMouseMove);
    svg.addEventListener('mouseup', onMouseUp);
    svg.addEventListener('mouseleave', onMouseUp);
    svg.addEventListener('wheel', onWheel, { passive: false });
    svg.addEventListener('contextmenu', onContextMenu);
    document.addEventListener('keydown', onKeyDown);
    document.addEventListener('click', hideContextMenu);

    // Formula selector
    if (formulaSelect) {
      formulaSelect.addEventListener('change', function () {
        if (state.isDirty && !confirm('Discard unsaved changes?')) {
          formulaSelect.value = state.formula ? state.formula.name : '';
          return;
        }
        var name = formulaSelect.value;
        if (name) loadFormula(name);
      });
    }

    // Palette drag
    setupPaletteDrag();

    // Load formula list and check URL params
    loadFormulaList().then(function () {
      var params = new URLSearchParams(window.location.search);
      var fname = params.get('formula');
      if (fname) {
        loadFormula(fname);
      } else if (params.get('new') === '1') {
        loadNew();
      }
    });

    // Load tool names
    loadToolList();
  }

  // ── Formula Loading ──────────────────────────────────────────────

  function loadFormulaList() {
    return BL.fetchApi('/api/v1/formulas').then(function (data) {
      if (!formulaSelect) return;
      var formulas = data.formulas || [];
      formulas.forEach(function (f) {
        var opt = document.createElement('option');
        opt.value = f.name;
        opt.textContent = f.display_name || f.name;
        if (f.is_system) opt.textContent += ' (system)';
        formulaSelect.appendChild(opt);
      });
    }).catch(function () {});
  }

  function loadFormula(name) {
    BL.fetchApi('/api/v1/formulas/' + encodeURIComponent(name)).then(function (data) {
      state.formula = data;
      state.definition = data.definition || { formula: {}, parameters: [], steps: [], on_failure: null };
      state.isReadOnly = !!data.is_system;
      state.isNew = false;
      originalFormulaName = data.name;
      state.isDirty = false;
      state.selectedId = null;
      state.selectedEdgeIdx = -1;
      state.history = [];
      state.historyIndex = -1;
      state.errors = [];
      state.cycleEdges = [];

      var graph = WFS.formulaToGraph(data);
      state.nodes = graph.nodes;
      state.edges = graph.edges;

      if (!restorePositions()) {
        autoLayout();
      }

      if (titleEl) titleEl.textContent = data.display_name || data.name || 'Workflow Editor';
      if (formulaSelect) formulaSelect.value = name;
      updateEmptyState();
      pushHistory();
      render();
      updateProps();
      validate();
    }).catch(function (e) {
      toast('Failed to load formula: ' + e, 'error');
    });
  }

  function loadNew() {
    state.formula = {
      name: '',
      display_name: '',
      description: '',
      is_system: false,
    };
    state.definition = {
      formula: { name: '', version: '1', description: '' },
      parameters: [],
      steps: [],
      on_failure: null,
    };
    state.isReadOnly = false;
    state.isNew = true;
    state.isDirty = false;
    state.selectedId = null;
    state.selectedEdgeIdx = -1;
    state.history = [];
    state.historyIndex = -1;
    state.errors = [];
    state.cycleEdges = [];
    state.nodes = [
      { _id: 'n_start', type: 'start', name: 'Start', _x: 400, _y: 40 },
      { _id: 'n_end', type: 'end', name: 'End', _x: 400, _y: 200 },
    ];
    state.edges = [];

    if (titleEl) titleEl.textContent = 'New Workflow';
    if (formulaSelect) formulaSelect.value = '';
    updateEmptyState();
    pushHistory();
    render();
    updateProps();
  }

  function loadToolList() {
    BL.fetchApi('/api/v1/formulas').then(function (data) {
      var tools = {};
      (data.formulas || []).forEach(function (f) {
        if (!f.definition) return;
        var steps = [];
        // Some list responses include definition, some don't
        if (f.definition && f.definition.steps) steps = f.definition.steps;
        steps.forEach(function (s) {
          (s.tools || []).forEach(function (t) { tools[t] = true; });
        });
      });
      // Merge with known common tools
      var common = [
        'store_memory', 'recall_memory', 'semantic_search', 'log_work',
        'create_task', 'list_tasks', 'claim_task', 'complete_task',
        'log_decision', 'beads_create_issue', 'beads_list_issues',
        'beads_run_formula', 'beads_list_formulas', 'read_file', 'write_file',
        'send_message', 'read_messages', 'health_check', 'ping',
      ];
      common.forEach(function (t) { tools[t] = true; });
      state.allTools = Object.keys(tools).sort();
    }).catch(function () {
      state.allTools = [];
    });
  }

  // ── Rendering ────────────────────────────────────────────────────

  function render() {
    while (canvasGroup.firstChild) canvasGroup.removeChild(canvasGroup.firstChild);
    canvasGroup.setAttribute('transform', 'translate(' + state.panX + ',' + state.panY + ') scale(' + state.zoom + ')');

    // Update grid pattern transform
    var gridPat = document.getElementById('wfe-grid');
    if (gridPat) {
      gridPat.setAttribute('patternTransform', 'translate(' + state.panX + ',' + state.panY + ') scale(' + state.zoom + ')');
    }

    // Parallel group backgrounds
    var groups = {};
    state.nodes.forEach(function (n) {
      if (n.group != null && (n.type === 'step' || n.type === 'condition')) {
        if (!groups[n.group]) groups[n.group] = [];
        groups[n.group].push(n);
      }
    });
    Object.keys(groups).forEach(function (gid) {
      if (groups[gid].length < 2) return;
      renderGroup(canvasGroup, groups[gid], gid);
    });

    // Edges
    state.edges.forEach(function (e, i) {
      renderEdge(canvasGroup, e, i);
    });

    // Edge preview (during drawing)
    if (state.edgeDraw) {
      renderEdgePreview(canvasGroup, state.edgeDraw);
    }

    // Nodes
    state.nodes.forEach(function (n) {
      switch (n.type) {
        case 'start': renderStartNode(canvasGroup, n); break;
        case 'end': renderEndNode(canvasGroup, n); break;
        case 'condition': renderConditionNode(canvasGroup, n); break;
        default: renderStepNode(canvasGroup, n);
      }
    });

    // Zoom display
    if (zoomDisplay) zoomDisplay.textContent = Math.round(state.zoom * 100) + '%';
  }

  function renderStepNode(parent, node) {
    var sel = node._id === state.selectedId;
    var err = hasError(node.name);
    var cls = 'wfe-node' + (sel ? ' selected' : '') + (err ? ' error' : '');
    var g = svgEl('g', { class: cls, 'data-id': node._id });

    // Background rect
    g.appendChild(svgEl('rect', { x: node._x, y: node._y, width: NODE_W, height: NODE_H }));

    // Name
    var nameEl = svgEl('text', {
      class: 'wfe-node-name', x: node._x + 10, y: node._y + 22,
    });
    nameEl.textContent = truncate(node.name, 22);
    g.appendChild(nameEl);

    // Role badge
    var rc = ROLE_COLORS[node.agent_role] || ROLE_COLORS.worker;
    var roleLen = (node.agent_role || 'worker').length * 6.5 + 14;
    g.appendChild(svgEl('rect', {
      class: 'wfe-role-badge', x: node._x + 10, y: node._y + 30,
      width: roleLen, height: 16, rx: 3, fill: rc.bg,
    }));
    var roleText = svgEl('text', {
      class: 'wfe-role-text', x: node._x + 17, y: node._y + 42, fill: rc.fg,
    });
    roleText.textContent = node.agent_role || 'worker';
    g.appendChild(roleText);

    // Tool count badge
    var tc = (node.tools || []).length;
    if (tc > 0) {
      g.appendChild(svgEl('circle', {
        class: 'wfe-tool-badge', cx: node._x + NODE_W - 18, cy: node._y + 18, r: 10,
      }));
      var tcText = svgEl('text', {
        class: 'wfe-tool-badge-text',
        x: node._x + NODE_W - 18, y: node._y + 22, 'text-anchor': 'middle',
      });
      tcText.textContent = String(tc);
      g.appendChild(tcText);
    }

    // When badge (small diamond + text)
    if (node.when) {
      g.appendChild(svgEl('rect', {
        class: 'wfe-when-badge', x: node._x + roleLen + 16, y: node._y + 30,
        width: Math.min((node.when.length * 5.5) + 10, NODE_W - roleLen - 30), height: 16,
        rx: 3, fill: 'rgba(245,158,11,0.12)',
      }));
      var whenText = svgEl('text', {
        class: 'wfe-when-text', x: node._x + roleLen + 21, y: node._y + 42,
      });
      whenText.textContent = truncate(node.when, 16);
      g.appendChild(whenText);
    }

    // Input port (top center)
    g.appendChild(svgEl('circle', {
      class: 'wfe-port', cx: node._x + NODE_W / 2, cy: node._y, r: PORT_R,
      'data-id': node._id, 'data-port': 'in',
    }));

    // Output port (bottom center)
    g.appendChild(svgEl('circle', {
      class: 'wfe-port', cx: node._x + NODE_W / 2, cy: node._y + NODE_H, r: PORT_R,
      'data-id': node._id, 'data-port': 'out',
    }));

    parent.appendChild(g);
  }

  function renderStartNode(parent, node) {
    var sel = node._id === state.selectedId;
    var g = svgEl('g', { class: 'wfe-node-start' + (sel ? ' selected' : ''), 'data-id': node._id });
    g.appendChild(svgEl('circle', { cx: node._x, cy: node._y, r: START_R }));
    var t = svgEl('text', { x: node._x, y: node._y + 4, 'text-anchor': 'middle' });
    t.textContent = 'Start';
    g.appendChild(t);
    // Output port
    g.appendChild(svgEl('circle', {
      class: 'wfe-port', cx: node._x, cy: node._y + START_R, r: PORT_R,
      'data-id': node._id, 'data-port': 'out',
    }));
    parent.appendChild(g);
  }

  function renderEndNode(parent, node) {
    var sel = node._id === state.selectedId;
    var g = svgEl('g', { class: 'wfe-node-end' + (sel ? ' selected' : ''), 'data-id': node._id });
    g.appendChild(svgEl('circle', { cx: node._x, cy: node._y, r: END_R }));
    var t = svgEl('text', { x: node._x, y: node._y + 4, 'text-anchor': 'middle' });
    t.textContent = 'End';
    g.appendChild(t);
    // Input port
    g.appendChild(svgEl('circle', {
      class: 'wfe-port', cx: node._x, cy: node._y - END_R, r: PORT_R,
      'data-id': node._id, 'data-port': 'in',
    }));
    parent.appendChild(g);
  }

  function renderConditionNode(parent, node) {
    var sel = node._id === state.selectedId;
    var err = hasError(node.name);
    var cls = 'wfe-node-condition' + (sel ? ' selected' : '') + (err ? ' error' : '');
    var g = svgEl('g', { class: cls, 'data-id': node._id });
    var cx = node._x, cy = node._y;
    var pts = cx + ',' + (cy - COND_SIZE) + ' ' +
              (cx + COND_SIZE) + ',' + cy + ' ' +
              cx + ',' + (cy + COND_SIZE) + ' ' +
              (cx - COND_SIZE) + ',' + cy;
    g.appendChild(svgEl('polygon', { points: pts }));
    var t = svgEl('text', { x: cx, y: cy - 6, 'text-anchor': 'middle', class: 'wfe-node-name' });
    t.textContent = truncate(node.name, 14);
    g.appendChild(t);
    var wt = svgEl('text', { x: cx, y: cy + 10, 'text-anchor': 'middle', fill: '#f59e0b', 'font-size': '9px' });
    wt.textContent = truncate(node.when || 'when?', 18);
    g.appendChild(wt);
    // Input port (top)
    g.appendChild(svgEl('circle', {
      class: 'wfe-port', cx: cx, cy: cy - COND_SIZE, r: PORT_R,
      'data-id': node._id, 'data-port': 'in',
    }));
    // Output port (bottom)
    g.appendChild(svgEl('circle', {
      class: 'wfe-port', cx: cx, cy: cy + COND_SIZE, r: PORT_R,
      'data-id': node._id, 'data-port': 'out',
    }));
    parent.appendChild(g);
  }

  function renderEdge(parent, edge, idx) {
    var from = nodeById(edge.from);
    var to = nodeById(edge.to);
    if (!from || !to) return;
    var p1 = portPos(from, 'out');
    var p2 = portPos(to, 'in');
    var cy = (p1.y + p2.y) / 2;
    var d = 'M' + p1.x + ',' + p1.y + ' C' + p1.x + ',' + cy + ' ' + p2.x + ',' + cy + ' ' + p2.x + ',' + p2.y;
    var isCycle = state.cycleEdges.some(function (c) { return c.from === edge.from && c.to === edge.to; });
    var isSelected = idx === state.selectedEdgeIdx;
    var cls = 'wfe-edge' + (isSelected ? ' selected' : '') + (isCycle ? ' wfe-edge-cycle' : '');
    if (from.type === 'condition') cls += ' wfe-edge-conditional';
    var marker = isCycle ? markerUrl('wfe-arrowhead-red')
                 : isSelected ? markerUrl('wfe-arrowhead-accent')
                 : markerUrl('wfe-arrowhead');
    var path = svgEl('path', { class: cls, d: d, 'data-edge-idx': idx, 'marker-end': marker });
    parent.appendChild(path);
  }

  function renderEdgePreview(parent, ed) {
    var from = nodeById(ed.fromId);
    if (!from) return;
    var p1 = portPos(from, ed.fromPort);
    var cy = (p1.y + ed.y) / 2;
    var d = 'M' + p1.x + ',' + p1.y + ' C' + p1.x + ',' + cy + ' ' + ed.x + ',' + cy + ' ' + ed.x + ',' + ed.y;
    parent.appendChild(svgEl('path', { class: 'wfe-edge-preview', d: d }));
  }

  function renderGroup(parent, members, gid) {
    var minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
    members.forEach(function (m) {
      var x1 = m.type === 'condition' ? m._x - COND_SIZE : m._x;
      var y1 = m.type === 'condition' ? m._y - COND_SIZE : m._y;
      var x2 = m.type === 'condition' ? m._x + COND_SIZE : m._x + NODE_W;
      var y2 = m.type === 'condition' ? m._y + COND_SIZE : m._y + NODE_H;
      if (x1 < minX) minX = x1;
      if (y1 < minY) minY = y1;
      if (x2 > maxX) maxX = x2;
      if (y2 > maxY) maxY = y2;
    });
    parent.appendChild(svgEl('rect', {
      class: 'wfe-group-rect',
      x: minX - GROUP_PAD, y: minY - GROUP_PAD - 14,
      width: maxX - minX + GROUP_PAD * 2, height: maxY - minY + GROUP_PAD * 2 + 14,
    }));
    var label = svgEl('text', {
      class: 'wfe-group-label', x: minX - GROUP_PAD + 6, y: minY - GROUP_PAD - 2,
    });
    label.textContent = 'Parallel Group ' + gid;
    parent.appendChild(label);
  }

  // ── Port position helpers ────────────────────────────────────────

  function portPos(node, port) {
    if (node.type === 'start') {
      return port === 'out' ? { x: node._x, y: node._y + START_R } : { x: node._x, y: node._y - START_R };
    }
    if (node.type === 'end') {
      return port === 'in' ? { x: node._x, y: node._y - END_R } : { x: node._x, y: node._y + END_R };
    }
    if (node.type === 'condition') {
      return port === 'in' ? { x: node._x, y: node._y - COND_SIZE } : { x: node._x, y: node._y + COND_SIZE };
    }
    // Step
    return port === 'in' ? { x: node._x + NODE_W / 2, y: node._y } : { x: node._x + NODE_W / 2, y: node._y + NODE_H };
  }

  // ── Mouse Interaction ────────────────────────────────────────────

  function onMouseDown(e) {
    hideContextMenu();

    // Check if clicking a port
    var portEl = findPort(e.target);
    if (portEl && !state.isReadOnly) {
      var pid = portEl.getAttribute('data-id');
      var pport = portEl.getAttribute('data-port');
      if (pport === 'out') {
        var fromNode = nodeById(pid);
        if (fromNode) {
          var pos = portPos(fromNode, 'out');
          state.edgeDraw = { fromId: pid, fromPort: 'out', x: pos.x, y: pos.y };
          e.preventDefault();
          return;
        }
      }
    }

    // Check if clicking a node
    var nodeG = findNodeGroup(e.target);
    if (nodeG) {
      var nid = nodeG.getAttribute('data-id');
      state.selectedId = nid;
      state.selectedEdgeIdx = -1;
      updateProps();

      if (!state.isReadOnly) {
        var node = nodeById(nid);
        if (node) {
          var pt = canvasPoint(e);
          var ox = node.type === 'step' ? node._x : node._x;
          var oy = node.type === 'step' ? node._y : node._y;
          state.dragState = { type: 'node', id: nid, startX: pt.x, startY: pt.y, offsetX: ox, offsetY: oy };
        }
      }
      render();
      e.preventDefault();
      return;
    }

    // Check if clicking an edge
    var edgeEl = findEdge(e.target);
    if (edgeEl) {
      var eidx = parseInt(edgeEl.getAttribute('data-edge-idx'), 10);
      state.selectedEdgeIdx = eidx;
      state.selectedId = null;
      updateProps();
      render();
      e.preventDefault();
      return;
    }

    // Click on canvas → pan or deselect
    state.selectedId = null;
    state.selectedEdgeIdx = -1;
    updateProps();
    state.dragState = {
      type: 'pan',
      startPanX: state.panX,
      startPanY: state.panY,
      startClientX: e.clientX,
      startClientY: e.clientY,
    };
    render();
    e.preventDefault();
  }

  function onMouseMove(e) {
    if (state.edgeDraw) {
      var pt = canvasPoint(e);
      state.edgeDraw.x = pt.x;
      state.edgeDraw.y = pt.y;
      render();
      return;
    }

    if (!state.dragState) return;

    if (state.dragState.type === 'node') {
      var pt2 = canvasPoint(e);
      var dx = pt2.x - state.dragState.startX;
      var dy = pt2.y - state.dragState.startY;
      var node = nodeById(state.dragState.id);
      if (node) {
        var nx = state.dragState.offsetX + dx;
        var ny = state.dragState.offsetY + dy;
        if (state.snapToGrid) {
          nx = Math.round(nx / GRID_SIZE) * GRID_SIZE;
          ny = Math.round(ny / GRID_SIZE) * GRID_SIZE;
        }
        node._x = nx;
        node._y = ny;
        render();
      }
    } else if (state.dragState.type === 'pan') {
      state.panX = state.dragState.startPanX + (e.clientX - state.dragState.startClientX);
      state.panY = state.dragState.startPanY + (e.clientY - state.dragState.startClientY);
      render();
    }
  }

  function onMouseUp(e) {
    if (state.edgeDraw) {
      // Check if dropping on a port
      var portEl = findPort(e.target);
      if (portEl) {
        var toId = portEl.getAttribute('data-id');
        var toPort = portEl.getAttribute('data-port');
        if (toPort === 'in' && toId !== state.edgeDraw.fromId) {
          // Validate: no duplicate edge
          var exists = state.edges.some(function (ed) {
            return ed.from === state.edgeDraw.fromId && ed.to === toId;
          });
          if (!exists) {
            pushHistory();
            state.edges.push({ from: state.edgeDraw.fromId, to: toId });
            state.isDirty = true;
            validate();
          }
        }
      }
      state.edgeDraw = null;
      render();
      return;
    }

    if (state.dragState && state.dragState.type === 'node') {
      // Check if position actually changed
      var node = nodeById(state.dragState.id);
      if (node && (node._x !== state.dragState.offsetX || node._y !== state.dragState.offsetY)) {
        pushHistory();
        state.isDirty = true;
        savePositions();
      }
    }

    state.dragState = null;
  }

  function onWheel(e) {
    e.preventDefault();
    var oldZoom = state.zoom;
    var factor = e.deltaY < 0 ? 1.1 : 0.9;
    state.zoom = Math.max(0.2, Math.min(3.0, state.zoom * factor));

    // Zoom toward cursor
    var rect = svg.getBoundingClientRect();
    var mx = e.clientX - rect.left;
    var my = e.clientY - rect.top;
    state.panX = mx - (mx - state.panX) * (state.zoom / oldZoom);
    state.panY = my - (my - state.panY) * (state.zoom / oldZoom);

    render();
  }

  function onContextMenu(e) {
    e.preventDefault();
    if (state.isReadOnly) return;

    var items = [];
    var nodeG = findNodeGroup(e.target);
    var edgeEl = findEdge(e.target);

    if (nodeG) {
      var nid = nodeG.getAttribute('data-id');
      var node = nodeById(nid);
      state.selectedId = nid;
      state.selectedEdgeIdx = -1;
      updateProps();
      render();

      if (node && node.type !== 'start' && node.type !== 'end') {
        items.push({ label: 'Duplicate', action: function () { duplicateNode(nid); } });
        items.push({ label: 'Disconnect All', action: function () { disconnectNode(nid); } });
        items.push({ separator: true });
        items.push({ label: 'Delete', danger: true, action: function () { deleteNode(nid); } });
      }
    } else if (edgeEl) {
      var eidx = parseInt(edgeEl.getAttribute('data-edge-idx'), 10);
      state.selectedEdgeIdx = eidx;
      state.selectedId = null;
      updateProps();
      render();
      items.push({ label: 'Delete Edge', danger: true, action: function () { deleteEdge(eidx); } });
    } else {
      items.push({ label: 'Add Step', action: function () { addStepAt(canvasPoint(e)); } });
      items.push({ label: 'Add Condition', action: function () { addConditionAt(canvasPoint(e)); } });
      items.push({ separator: true });
      items.push({ label: 'Auto Layout', action: function () { autoLayout(); render(); savePositions(); } });
      items.push({ label: 'Zoom to Fit', action: function () { zoomToFit(); } });
    }

    if (items.length > 0) showContextMenu(e.clientX, e.clientY, items);
  }

  // ── Keyboard ─────────────────────────────────────────────────────

  function onKeyDown(e) {
    if (e.target.tagName === 'INPUT' || e.target.tagName === 'TEXTAREA' || e.target.tagName === 'SELECT') return;

    if (e.key === 'Delete' || e.key === 'Backspace') {
      if (state.selectedId && !state.isReadOnly) {
        var n = nodeById(state.selectedId);
        if (n && n.type !== 'start' && n.type !== 'end') {
          deleteNode(state.selectedId);
          e.preventDefault();
        }
      } else if (state.selectedEdgeIdx >= 0 && !state.isReadOnly) {
        deleteEdge(state.selectedEdgeIdx);
        e.preventDefault();
      }
    } else if (e.key === 'z' && (e.ctrlKey || e.metaKey) && e.shiftKey) {
      redo();
      e.preventDefault();
    } else if (e.key === 'z' && (e.ctrlKey || e.metaKey)) {
      undo();
      e.preventDefault();
    } else if (e.key === 's' && (e.ctrlKey || e.metaKey)) {
      save();
      e.preventDefault();
    } else if (e.key === 'Escape') {
      hideContextMenu();
      state.selectedId = null;
      state.selectedEdgeIdx = -1;
      updateProps();
      render();
    } else if (e.key === 'Tab') {
      e.preventDefault();
      var stepNodes = state.nodes.filter(function (n) { return n.type === 'step' || n.type === 'condition'; });
      if (stepNodes.length === 0) return;
      var curIdx = -1;
      if (state.selectedId) {
        curIdx = stepNodes.findIndex(function (n) { return n._id === state.selectedId; });
      }
      var nextIdx = e.shiftKey ? (curIdx - 1 + stepNodes.length) % stepNodes.length
                               : (curIdx + 1) % stepNodes.length;
      state.selectedId = stepNodes[nextIdx]._id;
      state.selectedEdgeIdx = -1;
      updateProps();
      render();
    }
  }

  // ── Node Operations ──────────────────────────────────────────────

  function addStep() {
    if (state.isReadOnly) return;
    var cx = (-state.panX + 400) / state.zoom;
    var cy = (-state.panY + 300) / state.zoom;
    addStepAt({ x: cx, y: cy });
  }

  function addCondition() {
    if (state.isReadOnly) return;
    var cx = (-state.panX + 400) / state.zoom;
    var cy = (-state.panY + 300) / state.zoom;
    addConditionAt({ x: cx, y: cy });
  }

  function addStepAt(pos) {
    pushHistory();
    var n = stepCount() + 1;
    var node = {
      _id: 'n_' + Date.now() + '_' + n,
      type: 'step',
      name: 'step_' + n,
      agent_role: 'worker',
      tools: [],
      prompt: '',
      output: 'output_' + n,
      input: null,
      when: null,
      retries: null,
      backoff: null,
      timeout_seconds: null,
      group: null,
      examples: null,
      system_prompt: null,
      output_schema: null,
      model_domain: null,
      _x: pos.x,
      _y: pos.y,
    };
    if (state.snapToGrid) {
      node._x = Math.round(node._x / GRID_SIZE) * GRID_SIZE;
      node._y = Math.round(node._y / GRID_SIZE) * GRID_SIZE;
    }
    state.nodes.push(node);
    state.selectedId = node._id;
    state.selectedEdgeIdx = -1;
    state.isDirty = true;
    updateEmptyState();
    render();
    updateProps();
    validate();
  }

  function addConditionAt(pos) {
    pushHistory();
    var n = stepCount() + 1;
    var node = {
      _id: 'n_cond_' + Date.now() + '_' + n,
      type: 'condition',
      name: 'condition_' + n,
      agent_role: 'analyst',
      tools: [],
      prompt: 'Evaluate the condition',
      output: 'condition_result_' + n,
      input: null,
      when: 'true',
      retries: null,
      backoff: null,
      timeout_seconds: null,
      group: null,
      examples: null,
      system_prompt: null,
      output_schema: null,
      model_domain: null,
      _x: pos.x,
      _y: pos.y,
    };
    if (state.snapToGrid) {
      node._x = Math.round(node._x / GRID_SIZE) * GRID_SIZE;
      node._y = Math.round(node._y / GRID_SIZE) * GRID_SIZE;
    }
    state.nodes.push(node);
    state.selectedId = node._id;
    state.selectedEdgeIdx = -1;
    state.isDirty = true;
    updateEmptyState();
    render();
    updateProps();
    validate();
  }

  function deleteNode(nid) {
    var node = nodeById(nid);
    if (!node || node.type === 'start' || node.type === 'end') return;
    pushHistory();
    state.nodes = state.nodes.filter(function (n) { return n._id !== nid; });
    state.edges = state.edges.filter(function (e) { return e.from !== nid && e.to !== nid; });
    if (state.selectedId === nid) state.selectedId = null;
    state.isDirty = true;
    updateEmptyState();
    render();
    updateProps();
    validate();
  }

  function duplicateNode(nid) {
    var orig = nodeById(nid);
    if (!orig || orig.type === 'start' || orig.type === 'end') return;
    pushHistory();
    var dup = JSON.parse(JSON.stringify(orig));
    dup._id = 'n_dup_' + Date.now();
    dup.name = orig.name + '_copy';
    dup.output = orig.output + '_copy';
    dup._x += 40;
    dup._y += 40;
    state.nodes.push(dup);
    state.selectedId = dup._id;
    state.isDirty = true;
    render();
    updateProps();
    validate();
  }

  function disconnectNode(nid) {
    pushHistory();
    state.edges = state.edges.filter(function (e) { return e.from !== nid && e.to !== nid; });
    state.isDirty = true;
    render();
    validate();
  }

  function deleteEdge(idx) {
    if (idx < 0 || idx >= state.edges.length) return;
    pushHistory();
    state.edges.splice(idx, 1);
    state.selectedEdgeIdx = -1;
    state.isDirty = true;
    render();
    validate();
  }

  // ── Palette Drag ─────────────────────────────────────────────────

  function setupPaletteDrag() {
    var items = document.querySelectorAll('.wfe-palette-item');
    items.forEach(function (item) {
      item.addEventListener('dragstart', function (e) {
        e.dataTransfer.setData('text/plain', item.getAttribute('data-type'));
        e.dataTransfer.effectAllowed = 'copy';
      });
    });

    if (canvasContainer) {
      canvasContainer.addEventListener('dragover', function (e) {
        e.preventDefault();
        e.dataTransfer.dropEffect = 'copy';
      });
      canvasContainer.addEventListener('drop', function (e) {
        e.preventDefault();
        var type = e.dataTransfer.getData('text/plain');
        if (!type || state.isReadOnly) return;
        var pos = canvasPointFromClient(e.clientX, e.clientY);
        if (type === 'step') addStepAt(pos);
        else if (type === 'condition') addConditionAt(pos);
      });
    }
  }

  // ── Property Panel ───────────────────────────────────────────────

  function updateProps() {
    if (!propsPanel) return;
    var disabled = state.isReadOnly ? ' disabled' : '';

    if (state.selectedId) {
      var node = nodeById(state.selectedId);
      if (!node) return;
      if (node.type === 'start' || node.type === 'end') {
        propsPanel.innerHTML = '<h4>' + esc(node.name) + '</h4>' +
          '<p style="color:var(--text-secondary);font-size:0.82rem;">' +
          (node.type === 'start' ? 'Entry point of the workflow. Cannot be edited.' : 'Exit point of the workflow. Cannot be edited.') +
          '</p>';
        return;
      }
      renderNodeProps(node, disabled);
    } else if (state.formula) {
      renderFormulaProps(disabled);
    } else {
      propsPanel.innerHTML = '<p style="color:var(--text-secondary);font-size:0.85rem;">Select a node to edit its properties.</p>';
    }
  }

  function renderNodeProps(node, disabled) {
    var inputDesc = resolveInputDisplay(node._id);
    var toolsHtml = renderToolsMultiSelect(node.tools || [], disabled);
    var otherSteps = state.nodes.filter(function (n) {
      return (n.type === 'step' || n.type === 'condition') && n._id !== node._id;
    });
    var errorHandlerOpts = '<option value="">none</option>' + otherSteps.map(function (n) {
      return '<option value="' + esc(n.name) + '">' + esc(n.name) + '</option>';
    }).join('');

    var schemaStr = '';
    if (node.output_schema) {
      try { schemaStr = JSON.stringify(node.output_schema, null, 2); }
      catch (ex) { schemaStr = String(node.output_schema); }
    }

    propsPanel.innerHTML =
      '<h4>' + (node.type === 'condition' ? 'Condition' : 'Step') + ': ' + esc(node.name) + '</h4>' +
      '<label>Name</label><input id="wfe-p-name" value="' + esc(node.name) + '"' + disabled + '>' +
      '<label>Agent Role</label>' +
      '<select id="wfe-p-role"' + disabled + '>' +
        roleOption('researcher', node.agent_role) +
        roleOption('writer', node.agent_role) +
        roleOption('architect', node.agent_role) +
        roleOption('developer', node.agent_role) +
        roleOption('tester', node.agent_role) +
        roleOption('analyst', node.agent_role) +
        roleOption('monitor', node.agent_role) +
        roleOption('worker', node.agent_role) +
      '</select>' +
      '<label>Tools</label>' + toolsHtml +
      '<label>Prompt</label><textarea id="wfe-p-prompt" rows="4"' + disabled + '>' + esc(node.prompt) + '</textarea>' +
      '<label>Output Variable</label><input id="wfe-p-output" value="' + esc(node.output) + '"' + disabled + '>' +
      '<label>Input (from edges)</label><input value="' + esc(inputDesc) + '" disabled>' +
      '<label>Condition (when)</label><input id="wfe-p-when" value="' + esc(node.when || '') + '"' + disabled + '>' +
      '<label>Parallel Group</label><input id="wfe-p-group" type="number" min="0" value="' + (node.group != null ? node.group : '') + '"' + disabled + '>' +
      '<details class="wfe-props-section"><summary>Advanced</summary>' +
        '<label>Retries</label><input id="wfe-p-retries" type="number" min="0" value="' + (node.retries != null ? node.retries : '') + '"' + disabled + '>' +
        '<label>Backoff</label><input id="wfe-p-backoff" value="' + esc(node.backoff != null ? String(node.backoff) : '') + '"' + disabled + ' placeholder="e.g. exponential">' +
        '<label>Timeout (sec)</label><input id="wfe-p-timeout" type="number" min="0" value="' + (node.timeout_seconds != null ? node.timeout_seconds : '') + '"' + disabled + '>' +
        '<label>System Prompt</label><textarea id="wfe-p-sysprompt" rows="2"' + disabled + '>' + esc(node.system_prompt || '') + '</textarea>' +
        '<label>Examples</label><textarea id="wfe-p-examples" rows="2"' + disabled + '>' + esc(node.examples || '') + '</textarea>' +
        '<label>Output Schema (JSON)</label><textarea id="wfe-p-schema" rows="3"' + disabled + '>' + esc(schemaStr) + '</textarea>' +
        '<label>Error Handler</label><select id="wfe-p-errhandler"' + disabled + '>' + errorHandlerOpts + '</select>' +
        '<label>Model Domain</label>' +
        '<select id="wfe-p-domain"' + disabled + '>' +
          '<option value=""' + (!node.model_domain ? ' selected' : '') + '>inherit</option>' +
          '<option value="code"' + (node.model_domain === 'code' ? ' selected' : '') + '>code</option>' +
          '<option value="general"' + (node.model_domain === 'general' ? ' selected' : '') + '>general</option>' +
        '</select>' +
      '</details>' +
      (state.isReadOnly ? '' :
        '<div class="wfe-props-actions">' +
          '<button class="btn btn-sm btn-danger" onclick="WorkflowEditor.deleteSelected()">Delete</button>' +
        '</div>');

    if (!state.isReadOnly) {
      bindChange('wfe-p-name', function (v) { node.name = v; markDirty(); render(); });
      bindChange('wfe-p-role', function (v) { node.agent_role = v; markDirty(); render(); });
      bindChange('wfe-p-prompt', function (v) { node.prompt = v; markDirty(); });
      bindChange('wfe-p-output', function (v) { node.output = v; markDirty(); render(); });
      bindChange('wfe-p-when', function (v) {
        node.when = v || null;
        var oldType = node.type;
        if (v && node.type === 'step') node.type = 'condition';
        else if (!v && node.type === 'condition') node.type = 'step';
        markDirty();
        render();
        if (node.type !== oldType) updateProps();
      });
      bindChange('wfe-p-group', function (v) { node.group = v !== '' ? parseInt(v, 10) : null; markDirty(); render(); });
      bindChange('wfe-p-retries', function (v) { node.retries = v !== '' ? parseInt(v, 10) : null; markDirty(); });
      bindChange('wfe-p-backoff', function (v) { node.backoff = v || null; markDirty(); });
      bindChange('wfe-p-timeout', function (v) { node.timeout_seconds = v !== '' ? parseInt(v, 10) : null; markDirty(); });
      bindChange('wfe-p-sysprompt', function (v) { node.system_prompt = v || null; markDirty(); });
      bindChange('wfe-p-examples', function (v) { node.examples = v || null; markDirty(); });
      bindChange('wfe-p-schema', function (v) {
        if (!v) { node.output_schema = null; markDirty(); return; }
        try { node.output_schema = JSON.parse(v); markDirty(); }
        catch (ex) { /* leave invalid, will show red on blur */ }
      });
      bindChange('wfe-p-domain', function (v) { node.model_domain = v || null; markDirty(); });
      bindChange('wfe-p-errhandler', function (v) { node.error_handler = v || null; markDirty(); });

      setupToolsInteraction(node);
    }
  }

  function renderFormulaProps(disabled) {
    var meta = state.definition ? state.definition.formula : {};
    var params = state.definition ? (state.definition.parameters || []) : [];
    var paramStr = params.map(function (p) {
      return (p.required ? '*' : '') + (p.name || '') + ' (' + (p.type || p.param_type || 'string') + ')';
    }).join(', ');

    propsPanel.innerHTML =
      '<h4>Formula Metadata</h4>' +
      '<label>Name (slug)</label><input id="wfe-m-name" value="' + esc(state.formula.name || meta.name || '') + '"' + disabled + '>' +
      '<label>Display Name</label><input id="wfe-m-display" value="' + esc(state.formula.display_name || '') + '"' + disabled + '>' +
      '<label>Description</label><textarea id="wfe-m-desc" rows="2"' + disabled + '>' + esc(state.formula.description || meta.description || '') + '</textarea>' +
      '<label>Version</label><input id="wfe-m-version" value="' + esc(meta.version || '1') + '"' + disabled + '>' +
      '<label>Model Domain</label>' +
      '<select id="wfe-m-domain"' + disabled + '>' +
        '<option value=""' + (!meta.model_domain ? ' selected' : '') + '>auto</option>' +
        '<option value="code"' + (meta.model_domain === 'code' ? ' selected' : '') + '>code</option>' +
        '<option value="general"' + (meta.model_domain === 'general' ? ' selected' : '') + '>general</option>' +
      '</select>' +
      '<label>Parameters</label><input value="' + esc(paramStr || 'none') + '" disabled>' +
      '<p style="font-size:0.78rem;color:var(--text-secondary);margin-top:0.5rem;">' +
        stepCount() + ' step(s). Click a node to edit.' +
      '</p>';

    if (!state.isReadOnly) {
      bindChange('wfe-m-name', function (v) {
        state.formula.name = v;
        if (state.definition && state.definition.formula) state.definition.formula.name = v;
        markDirty();
      });
      bindChange('wfe-m-display', function (v) {
        state.formula.display_name = v;
        if (state.definition && state.definition.formula) state.definition.formula.display_name = v;
        markDirty();
      });
      bindChange('wfe-m-desc', function (v) {
        state.formula.description = v;
        if (state.definition && state.definition.formula) state.definition.formula.description = v;
        markDirty();
      });
      bindChange('wfe-m-version', function (v) {
        if (state.definition && state.definition.formula) state.definition.formula.version = v;
        markDirty();
      });
      bindChange('wfe-m-domain', function (v) {
        if (state.definition && state.definition.formula) state.definition.formula.model_domain = v || null;
        markDirty();
      });
    }
  }

  // ── Searchable Tool Multi-Select ─────────────────────────────────

  function renderToolsMultiSelect(tools, disabled) {
    if (disabled) {
      return '<input value="' + esc((tools || []).join(', ')) + '" disabled>';
    }
    var html = '<div class="wfe-tools-input" id="wfe-tools-container">';
    (tools || []).forEach(function (t) {
      html += '<span class="wfe-tool-tag" data-tool="' + esc(t) + '">' +
        esc(t) + '<button class="wfe-tool-remove" data-tool="' + esc(t) + '">&times;</button></span>';
    });
    html += '<input type="text" id="wfe-tool-search" placeholder="Add tool..." autocomplete="off">';
    html += '</div>';
    html += '<div id="wfe-tool-dropdown" class="wfe-tool-dropdown" style="display:none"></div>';
    return html;
  }

  function setupToolsInteraction(node) {
    var searchInput = document.getElementById('wfe-tool-search');
    var dropdown = document.getElementById('wfe-tool-dropdown');
    var container = document.getElementById('wfe-tools-container');
    if (!searchInput || !dropdown || !container) return;

    // Remove buttons
    container.querySelectorAll('.wfe-tool-remove').forEach(function (btn) {
      btn.addEventListener('click', function (e) {
        e.preventDefault();
        var tool = btn.getAttribute('data-tool');
        node.tools = (node.tools || []).filter(function (t) { return t !== tool; });
        pushHistory();
        state.isDirty = true;
        updateProps();
        render();
        validate();
      });
    });

    // Search input
    searchInput.addEventListener('input', function () {
      var query = searchInput.value.toLowerCase().trim();
      if (!query) { dropdown.style.display = 'none'; return; }
      var matches = state.allTools.filter(function (t) {
        return t.toLowerCase().indexOf(query) >= 0 && (node.tools || []).indexOf(t) < 0;
      }).slice(0, 15);
      if (matches.length === 0) { dropdown.style.display = 'none'; return; }
      dropdown.innerHTML = matches.map(function (t) {
        return '<div class="wfe-tool-dropdown-item" data-tool="' + esc(t) + '">' + esc(t) + '</div>';
      }).join('');
      dropdown.style.display = 'block';
      dropdown.querySelectorAll('.wfe-tool-dropdown-item').forEach(function (item) {
        item.addEventListener('click', function () {
          var tool = item.getAttribute('data-tool');
          if (!node.tools) node.tools = [];
          node.tools.push(tool);
          pushHistory();
          state.isDirty = true;
          searchInput.value = '';
          dropdown.style.display = 'none';
          updateProps();
          render();
          validate();
        });
      });
    });

    searchInput.addEventListener('keydown', function (e) {
      if (e.key === 'Enter') {
        var v = searchInput.value.trim();
        if (v && (node.tools || []).indexOf(v) < 0) {
          if (!node.tools) node.tools = [];
          node.tools.push(v);
          pushHistory();
          state.isDirty = true;
          searchInput.value = '';
          dropdown.style.display = 'none';
          updateProps();
          render();
          validate();
        }
        e.preventDefault();
      } else if (e.key === 'Escape') {
        dropdown.style.display = 'none';
      }
    });

    searchInput.addEventListener('blur', function () {
      setTimeout(function () { dropdown.style.display = 'none'; }, 200);
    });

    // Focus container click
    container.addEventListener('click', function () { searchInput.focus(); });
  }

  // ── Validation ───────────────────────────────────────────────────

  var validateTimer = null;
  function validate() {
    clearTimeout(validateTimer);
    validateTimer = setTimeout(doValidate, 300);
  }

  function doValidate() {
    state.errors = [];
    var names = {};

    state.nodes.forEach(function (n) {
      if (n.type === 'start' || n.type === 'end') return;

      if (!n.name || !n.name.trim()) {
        state.errors.push({ step: n._id, msg: 'Step has no name' });
      } else if (names[n.name]) {
        state.errors.push({ step: n._id, msg: 'Duplicate name: ' + n.name });
      }
      names[n.name] = true;

      if (!n.prompt || !n.prompt.trim()) {
        state.errors.push({ step: n._id, msg: n.name + ': prompt is empty' });
      }
      if (!n.output || !n.output.trim()) {
        state.errors.push({ step: n._id, msg: n.name + ': output is empty' });
      }
    });

    // Cycle detection
    state.cycleEdges = WFS.detectCycles(state.nodes, state.edges);
    if (state.cycleEdges.length > 0) {
      state.errors.push({ step: null, msg: 'Circular dependency detected — cannot save' });
    }

    if (validationBar) {
      var sc = stepCount();
      if (state.errors.length === 0) {
        validationBar.className = 'wfe-validation valid';
        validationBar.textContent = sc + ' step(s), no errors';
      } else {
        validationBar.className = 'wfe-validation has-errors';
        validationBar.textContent = state.errors.length + ' error(s): ' + state.errors[0].msg;
      }
    }
  }

  function hasError(name) {
    return state.errors.some(function (e) {
      if (e.step) {
        var n = nodeById(e.step);
        return n && n.name === name;
      }
      return false;
    });
  }

  // ── Undo / Redo ──────────────────────────────────────────────────

  function pushHistory() {
    var snapshot = JSON.parse(JSON.stringify({ nodes: state.nodes, edges: state.edges }));
    // Truncate future
    if (state.historyIndex < state.history.length - 1) {
      state.history = state.history.slice(0, state.historyIndex + 1);
    }
    state.history.push(snapshot);
    if (state.history.length > MAX_HISTORY) state.history.shift();
    state.historyIndex = state.history.length - 1;
  }

  function undo() {
    if (state.historyIndex <= 0) return;
    state.historyIndex--;
    restoreSnapshot(state.history[state.historyIndex]);
  }

  function redo() {
    if (state.historyIndex >= state.history.length - 1) return;
    state.historyIndex++;
    restoreSnapshot(state.history[state.historyIndex]);
  }

  function restoreSnapshot(snap) {
    state.nodes = JSON.parse(JSON.stringify(snap.nodes));
    state.edges = JSON.parse(JSON.stringify(snap.edges));
    state.selectedId = null;
    state.selectedEdgeIdx = -1;
    state.isDirty = true;
    updateEmptyState();
    render();
    updateProps();
    validate();
  }

  // ── Auto Layout ──────────────────────────────────────────────────

  function autoLayout() {
    // Separate start/end from step nodes
    var startNode = state.nodes.find(function (n) { return n.type === 'start'; });
    var endNode = state.nodes.find(function (n) { return n.type === 'end'; });
    var stepNodes = state.nodes.filter(function (n) { return n.type === 'step' || n.type === 'condition'; });

    // Topological sort
    var ids = {};
    stepNodes.forEach(function (n) { ids[n._id] = true; });
    var inDeg = {};
    var adj = {};
    stepNodes.forEach(function (n) { inDeg[n._id] = 0; adj[n._id] = []; });
    state.edges.forEach(function (e) {
      if (ids[e.from] && ids[e.to]) {
        adj[e.from].push(e.to);
        inDeg[e.to] = (inDeg[e.to] || 0) + 1;
      }
    });

    var queue = [];
    stepNodes.forEach(function (n) {
      if (inDeg[n._id] === 0) queue.push(n._id);
    });

    var layers = [];
    while (queue.length > 0) {
      // Group by parallel group if applicable
      var layer = queue.splice(0, queue.length);
      layers.push(layer);
      var nextQ = [];
      layer.forEach(function (id) {
        (adj[id] || []).forEach(function (nb) {
          inDeg[nb]--;
          if (inDeg[nb] === 0) nextQ.push(nb);
        });
      });
      queue = nextQ;
    }

    // Position start node
    var centerX = 400;
    if (startNode) {
      startNode._x = centerX;
      startNode._y = 40;
    }

    // Position step layers
    layers.forEach(function (layer, li) {
      var totalW = layer.length * NODE_W + (layer.length - 1) * MARGIN_X;
      var startX = centerX - totalW / 2 + NODE_W / 2;
      layer.forEach(function (id, si) {
        var node = nodeById(id);
        if (!node) return;
        if (node.type === 'condition') {
          node._x = startX + si * (NODE_W + MARGIN_X) + NODE_W / 2;
          node._y = (li + 1) * LAYER_H + 40;
        } else {
          node._x = startX + si * (NODE_W + MARGIN_X);
          node._y = (li + 1) * LAYER_H + 40;
        }
      });
    });

    // Position end node
    if (endNode) {
      endNode._x = centerX;
      endNode._y = (layers.length + 1) * LAYER_H + 40;
    }
  }

  function zoomToFit() {
    if (state.nodes.length === 0) return;
    var minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
    state.nodes.forEach(function (n) {
      var x1, y1, x2, y2;
      if (n.type === 'start') { x1 = n._x - START_R; y1 = n._y - START_R; x2 = n._x + START_R; y2 = n._y + START_R; }
      else if (n.type === 'end') { x1 = n._x - END_R; y1 = n._y - END_R; x2 = n._x + END_R; y2 = n._y + END_R; }
      else if (n.type === 'condition') { x1 = n._x - COND_SIZE; y1 = n._y - COND_SIZE; x2 = n._x + COND_SIZE; y2 = n._y + COND_SIZE; }
      else { x1 = n._x; y1 = n._y; x2 = n._x + NODE_W; y2 = n._y + NODE_H; }
      if (x1 < minX) minX = x1;
      if (y1 < minY) minY = y1;
      if (x2 > maxX) maxX = x2;
      if (y2 > maxY) maxY = y2;
    });

    var pad = 80;
    var rect = svg.getBoundingClientRect();
    var canvasW = rect.width;
    var canvasH = rect.height;
    var graphW = maxX - minX + pad * 2;
    var graphH = maxY - minY + pad * 2;
    state.zoom = Math.min(canvasW / graphW, canvasH / graphH, 2);
    state.panX = (canvasW - graphW * state.zoom) / 2 - minX * state.zoom + pad * state.zoom;
    state.panY = (canvasH - graphH * state.zoom) / 2 - minY * state.zoom + pad * state.zoom;
    render();
  }

  function toggleSnap(on) {
    state.snapToGrid = !!on;
  }

  // ── Save / Delete / Run ──────────────────────────────────────────

  function save() {
    if (state.isReadOnly) { toast('System formulas are read-only', 'error'); return; }
    doValidate();
    if (state.errors.length > 0) {
      toast('Fix ' + state.errors.length + ' error(s) before saving', 'error');
      return;
    }

    var definition = WFS.graphToFormula(state);

    if (state.isNew) {
      var name = state.formula.name;
      if (!name) { toast('Formula name is required', 'error'); return; }
      var body = {
        name: name,
        display_name: state.formula.display_name || name,
        description: state.formula.description || '',
        definition: definition,
        tags: [],
      };
      BL.fetchApi('/api/v1/formulas', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      }).then(function () {
        state.isDirty = false;
        state.isNew = false;
        originalFormulaName = name;
        savePositions();
        toast('Formula "' + name + '" created', 'ok');
        // Update URL and refresh dropdown
        history.replaceState(null, '', '/workflows/?formula=' + encodeURIComponent(name));
        if (formulaSelect) {
          var opt = document.createElement('option');
          opt.value = name;
          opt.textContent = state.formula.display_name || name;
          formulaSelect.appendChild(opt);
          formulaSelect.value = name;
        }
      }).catch(function (e) { toast('Save failed: ' + e, 'error'); });
    } else {
      var saveName = originalFormulaName || state.formula.name;
      var payload = {
        definition: definition,
        display_name: state.formula.display_name,
        description: state.formula.description,
      };
      BL.fetchApi('/api/v1/formulas/' + encodeURIComponent(saveName) + '/update', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload),
      }).then(function () {
        state.isDirty = false;
        savePositions();
        toast('Formula "' + saveName + '" saved', 'ok');
      }).catch(function (e) { toast('Save failed: ' + e, 'error'); });
    }
  }

  function deleteFormula() {
    if (state.isReadOnly || !state.formula || !state.formula.name) return;
    if (!confirm('Disable formula "' + state.formula.name + '"? (Use Settings to re-enable)')) return;
    BL.fetchApi('/api/v1/formulas/' + encodeURIComponent(state.formula.name) + '/toggle', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: '{}',
    }).then(function () {
      toast('Formula disabled', 'ok');
    }).catch(function (e) { toast('Failed: ' + e, 'error'); });
  }

  function runFormula() {
    toast('Use the CLI or Beads tools to run formulas', 'info');
  }

  // ── Position Persistence ─────────────────────────────────────────

  function savePositions() {
    var key = state.formula ? state.formula.name : null;
    if (!key) return;
    var positions = {};
    state.nodes.forEach(function (n) {
      var name = n.type === 'start' ? '__start' : (n.type === 'end' ? '__end' : n.name);
      positions[name] = { x: n._x, y: n._y };
    });
    try { localStorage.setItem(STORAGE_PREFIX + key, JSON.stringify(positions)); }
    catch (ex) { /* quota exceeded */ }
  }

  function restorePositions() {
    var key = state.formula ? state.formula.name : null;
    if (!key) return false;
    try {
      var saved = localStorage.getItem(STORAGE_PREFIX + key);
      if (!saved) return false;
      var positions = JSON.parse(saved);
      var restored = 0;
      state.nodes.forEach(function (n) {
        var name = n.type === 'start' ? '__start' : (n.type === 'end' ? '__end' : n.name);
        if (positions[name]) {
          n._x = positions[name].x;
          n._y = positions[name].y;
          restored++;
        }
      });
      return restored > 0;
    } catch (ex) { return false; }
  }

  // ── Context Menu ─────────────────────────────────────────────────

  function showContextMenu(x, y, items) {
    if (!contextMenu) return;
    contextMenu.innerHTML = items.map(function (item) {
      if (item.separator) return '<div class="wfe-context-menu-separator"></div>';
      var cls = 'wfe-context-menu-item' + (item.danger ? ' danger' : '');
      return '<button class="' + cls + '">' + esc(item.label) + '</button>';
    }).join('');
    contextMenu.style.left = x + 'px';
    contextMenu.style.top = y + 'px';
    contextMenu.style.display = 'block';

    // Bind actions
    var btns = contextMenu.querySelectorAll('.wfe-context-menu-item');
    var actionIdx = 0;
    items.forEach(function (item, i) {
      if (item.separator) return;
      if (btns[actionIdx]) {
        btns[actionIdx].addEventListener('click', function () {
          hideContextMenu();
          item.action();
        });
      }
      actionIdx++;
    });
  }

  function hideContextMenu() {
    if (contextMenu) contextMenu.style.display = 'none';
  }

  // ── Toast ────────────────────────────────────────────────────────

  function toast(msg, type) {
    var el = document.createElement('div');
    el.className = 'wfe-toast ' + (type || '');
    el.textContent = msg;
    document.body.appendChild(el);
    setTimeout(function () { el.remove(); }, 3000);
  }

  // ── Helpers ──────────────────────────────────────────────────────

  function nodeById(id) {
    return state.nodes.find(function (n) { return n._id === id; });
  }

  function stepCount() {
    return state.nodes.filter(function (n) { return n.type === 'step' || n.type === 'condition'; }).length;
  }

  var historyTimer = null;
  function markDirty() {
    state.isDirty = true;
    // Debounce history snapshots so typing doesn't fill history with single-char changes
    clearTimeout(historyTimer);
    historyTimer = setTimeout(function () { pushHistory(); }, 400);
    validate();
  }

  // Absolute URL for SVG marker references (fragments break with URL hash)
  function markerUrl(id) {
    return 'url(' + window.location.pathname + window.location.search + '#' + id + ')';
  }

  function esc(s) { return BL.escapeHtml(s || ''); }

  function truncate(s, n) { return s && s.length > n ? s.substring(0, n - 1) + '\u2026' : (s || ''); }

  function svgEl(tag, attrs) {
    var el = document.createElementNS(NS, tag);
    if (attrs) Object.keys(attrs).forEach(function (k) { el.setAttribute(k, attrs[k]); });
    return el;
  }

  function bindChange(id, fn) {
    var el = document.getElementById(id);
    if (!el) return;
    el.addEventListener('input', function () { fn(el.value); });
  }

  function roleOption(role, current) {
    return '<option value="' + role + '"' + (current === role ? ' selected' : '') + '>' + role + '</option>';
  }

  function resolveInputDisplay(nodeId) {
    var sources = [];
    state.edges.forEach(function (e) {
      if (e.to === nodeId) {
        var src = nodeById(e.from);
        if (src && src.type !== 'start') sources.push(src.output || src.name);
        else if (src && src.type === 'start') sources.push('(start)');
      }
    });
    return sources.join(', ') || '(none)';
  }

  function canvasPoint(e) {
    var rect = svg.getBoundingClientRect();
    return {
      x: (e.clientX - rect.left - state.panX) / state.zoom,
      y: (e.clientY - rect.top - state.panY) / state.zoom,
    };
  }

  function canvasPointFromClient(cx, cy) {
    var rect = svg.getBoundingClientRect();
    return {
      x: (cx - rect.left - state.panX) / state.zoom,
      y: (cy - rect.top - state.panY) / state.zoom,
    };
  }

  function findPort(el) {
    while (el && el !== svg) {
      if (el.classList && el.classList.contains('wfe-port')) return el;
      el = el.parentElement;
    }
    return null;
  }

  function findNodeGroup(el) {
    while (el && el !== svg) {
      if (el.getAttribute && el.getAttribute('data-id') && el.tagName === 'g') return el;
      el = el.parentElement;
    }
    return null;
  }

  function findEdge(el) {
    if (el && el.classList && el.classList.contains('wfe-edge')) return el;
    return null;
  }

  function updateEmptyState() {
    if (!emptyState) return;
    var hasSteps = stepCount() > 0;
    emptyState.classList.toggle('hidden', hasSteps);
  }

  function deleteSelected() {
    if (state.selectedId) {
      var n = nodeById(state.selectedId);
      if (n && n.type !== 'start' && n.type !== 'end') deleteNode(state.selectedId);
    }
  }

  // ── Public API ───────────────────────────────────────────────────

  window.WorkflowEditor = {
    loadFormula: loadFormula,
    loadNew: loadNew,
    addStep: addStep,
    addCondition: addCondition,
    save: save,
    deleteFormula: deleteFormula,
    runFormula: runFormula,
    autoLayout: function () { pushHistory(); autoLayout(); state.isDirty = true; render(); savePositions(); },
    zoomToFit: zoomToFit,
    undo: undo,
    redo: redo,
    toggleSnap: toggleSnap,
    deleteSelected: deleteSelected,
  };

  // Auto-init
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})();
