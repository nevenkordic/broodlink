/*
 * Broodlink - Multi-agent AI orchestration system
 * Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
 * SPDX-License-Identifier: AGPL-3.0-or-later
 *
 * Visual Formula Editor — SVG-based graph builder for workflow formulas
 */

(function () {
  'use strict';

  var BL = window.Broodlink;
  var NS = 'http://www.w3.org/2000/svg';

  // Layout constants
  var NODE_W = 180, NODE_H = 64, MARGIN_X = 60, LAYER_H = 120;
  var PORT_R = 6, GROUP_PAD = 20;

  // ── State ──────────────────────────────────────────────────────────

  var state = {
    formula: null,        // Full formula data from API
    definition: null,     // definition object (formula, parameters, steps, on_failure)
    steps: [],            // Steps with layout positions [{...step, x, y}]
    selectedIndex: -1,
    isReadOnly: false,
    isDirty: false,
    isNew: false,
    dragState: null,      // {index, startX, startY, offsetX, offsetY}
    errors: [],
    panX: 40, panY: 40,
  };

  // ── DOM refs ────────────────────────────────────────────────────────

  var overlay, svg, propsPanel, validationBar, titleEl;

  function init() {
    overlay = document.getElementById('fe-overlay');
    svg = document.getElementById('fe-svg');
    propsPanel = document.getElementById('fe-props');
    validationBar = document.getElementById('fe-validation');
    titleEl = document.getElementById('fe-title');

    if (!overlay || !svg) return;

    svg.addEventListener('mousedown', onSvgMouseDown);
    svg.addEventListener('mousemove', onSvgMouseMove);
    svg.addEventListener('mouseup', onSvgMouseUp);
    svg.addEventListener('mouseleave', onSvgMouseUp);

    document.addEventListener('keydown', onKeyDown);
  }

  // ── Open / Close ──────────────────────────────────────────────────

  function open(formulaData) {
    if (!overlay) init();
    state.formula = formulaData;
    state.definition = formulaData.definition || { formula: {}, parameters: [], steps: [], on_failure: null };
    state.isReadOnly = !!formulaData.is_system;
    state.isNew = false;
    state.isDirty = false;
    state.selectedIndex = -1;
    state.errors = [];

    parseSteps();
    computeLayout();
    render();
    updateProps();
    validate();

    if (titleEl) titleEl.textContent = formulaData.display_name || formulaData.name || 'Formula';
    overlay.classList.add('active');
  }

  function openNew() {
    if (!overlay) init();
    state.formula = {
      name: '',
      display_name: '',
      description: '',
      is_system: false,
      definition: {
        formula: { name: '', version: '1', description: '' },
        parameters: [],
        steps: [{ name: 'step_1', agent_role: 'worker', tools: [], prompt: '', output: 'result', input: null }],
        on_failure: null,
      }
    };
    state.definition = state.formula.definition;
    state.isReadOnly = false;
    state.isNew = true;
    state.isDirty = false;
    state.selectedIndex = 0;
    state.errors = [];

    parseSteps();
    computeLayout();
    render();
    updateProps();
    validate();

    if (titleEl) titleEl.textContent = 'New Formula';
    overlay.classList.add('active');
  }

  function close() {
    if (state.isDirty && !confirm('Discard unsaved changes?')) return;
    overlay.classList.remove('active');
    state.formula = null;
    state.steps = [];
  }

  // ── Parse / Serialize ─────────────────────────────────────────────

  function parseSteps() {
    var raw = state.definition.steps || [];
    state.steps = raw.map(function (s, i) {
      return Object.assign({}, s, { _x: 0, _y: 0, _index: i });
    });
  }

  function serializeDefinition() {
    var steps = state.steps.map(function (s) {
      var step = {
        name: s.name,
        agent_role: s.agent_role,
        tools: s.tools && s.tools.length > 0 ? s.tools : null,
        prompt: s.prompt,
        input: s.input || null,
        output: s.output,
      };
      if (s.when) step.when = s.when;
      if (s.retries) step.retries = s.retries;
      if (s.backoff) step.backoff = s.backoff;
      if (s.timeout_seconds) step.timeout_seconds = s.timeout_seconds;
      if (s.group != null) step.group = s.group;
      if (s.examples) step.examples = s.examples;
      if (s.system_prompt) step.system_prompt = s.system_prompt;
      if (s.output_schema) step.output_schema = s.output_schema;
      if (s.model_domain) step.model_domain = s.model_domain;
      return step;
    });

    return {
      formula: state.definition.formula,
      parameters: state.definition.parameters || [],
      steps: steps,
      on_failure: state.definition.on_failure || null,
    };
  }

  // ── Layout ────────────────────────────────────────────────────────

  function computeLayout() {
    // Partition steps into layers: sequential steps get their own layer,
    // parallel group members share a layer.
    var layers = [];
    var currentLayer = [];
    var lastGroup = null;

    state.steps.forEach(function (s) {
      var g = s.group != null ? s.group : null;
      if (g !== null && g === lastGroup) {
        currentLayer.push(s);
      } else {
        if (currentLayer.length > 0) layers.push(currentLayer);
        currentLayer = [s];
        lastGroup = g;
      }
    });
    if (currentLayer.length > 0) layers.push(currentLayer);

    // Assign positions
    layers.forEach(function (layer, li) {
      var totalW = layer.length * NODE_W + (layer.length - 1) * MARGIN_X;
      var startX = (totalW > 0) ? -totalW / 2 + NODE_W / 2 : 0;
      layer.forEach(function (s, si) {
        s._x = startX + si * (NODE_W + MARGIN_X) + 400;
        s._y = li * LAYER_H + 60;
      });
    });
  }

  // ── Rendering ─────────────────────────────────────────────────────

  function render() {
    // Clear SVG
    while (svg.firstChild) svg.removeChild(svg.firstChild);

    var g = svgEl('g', { transform: 'translate(' + state.panX + ',' + state.panY + ')' });

    // Draw parallel group backgrounds
    var groups = {};
    state.steps.forEach(function (s) {
      if (s.group != null) {
        if (!groups[s.group]) groups[s.group] = [];
        groups[s.group].push(s);
      }
    });
    Object.keys(groups).forEach(function (gid) {
      var members = groups[gid];
      var minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
      members.forEach(function (m) {
        if (m._x < minX) minX = m._x;
        if (m._y < minY) minY = m._y;
        if (m._x + NODE_W > maxX) maxX = m._x + NODE_W;
        if (m._y + NODE_H > maxY) maxY = m._y + NODE_H;
      });
      var rect = svgEl('rect', {
        class: 'fe-group-rect',
        x: minX - GROUP_PAD,
        y: minY - GROUP_PAD - 14,
        width: maxX - minX + GROUP_PAD * 2,
        height: maxY - minY + GROUP_PAD * 2 + 14,
      });
      g.appendChild(rect);
      var label = svgEl('text', {
        class: 'fe-group-label',
        x: minX - GROUP_PAD + 6,
        y: minY - GROUP_PAD - 2,
      });
      label.textContent = 'Parallel Group ' + gid;
      g.appendChild(label);
    });

    // Draw edges
    state.steps.forEach(function (s) {
      if (!s.input) return;
      var inputs = typeof s.input === 'string' ? [s.input] : (Array.isArray(s.input) ? s.input : []);
      inputs.forEach(function (inp) {
        var source = state.steps.find(function (o) { return o.output === inp; });
        if (!source) return;
        var x1 = source._x + NODE_W / 2;
        var y1 = source._y + NODE_H;
        var x2 = s._x + NODE_W / 2;
        var y2 = s._y;
        var cy = (y1 + y2) / 2;
        var path = svgEl('path', {
          class: 'fe-edge',
          d: 'M' + x1 + ',' + y1 + ' C' + x1 + ',' + cy + ' ' + x2 + ',' + cy + ' ' + x2 + ',' + y2,
        });
        g.appendChild(path);
      });
    });

    // Draw nodes
    state.steps.forEach(function (s, i) {
      var nodeG = svgEl('g', {
        class: 'fe-node' + (i === state.selectedIndex ? ' selected' : '') + (hasError(s.name) ? ' error' : ''),
        'data-index': i,
      });

      var rect = svgEl('rect', { x: s._x, y: s._y, width: NODE_W, height: NODE_H });
      nodeG.appendChild(rect);

      var nameText = svgEl('text', {
        class: 'fe-node-name',
        x: s._x + NODE_W / 2,
        y: s._y + 26,
        'text-anchor': 'middle',
      });
      nameText.textContent = truncate(s.name, 18);
      nodeG.appendChild(nameText);

      var roleText = svgEl('text', {
        class: 'fe-node-role',
        x: s._x + NODE_W / 2,
        y: s._y + 44,
        'text-anchor': 'middle',
      });
      roleText.textContent = s.agent_role || '';
      nodeG.appendChild(roleText);

      // Input port (top center)
      if (i > 0) {
        var inPort = svgEl('circle', {
          class: 'fe-port',
          cx: s._x + NODE_W / 2,
          cy: s._y,
          r: PORT_R,
        });
        nodeG.appendChild(inPort);
      }

      // Output port (bottom center)
      var outPort = svgEl('circle', {
        class: 'fe-port',
        cx: s._x + NODE_W / 2,
        cy: s._y + NODE_H,
        r: PORT_R,
      });
      nodeG.appendChild(outPort);

      g.appendChild(nodeG);
    });

    svg.appendChild(g);
  }

  function hasError(name) {
    return state.errors.some(function (e) { return e.step === name; });
  }

  // ── Property Panel ────────────────────────────────────────────────

  function updateProps() {
    if (!propsPanel) return;
    var disabled = state.isReadOnly ? ' disabled' : '';

    if (state.selectedIndex < 0) {
      // Show formula metadata
      var meta = state.definition.formula || {};
      var params = state.definition.parameters || [];
      var paramStr = params.map(function (p) {
        return (p.required ? '*' : '') + p.name + ' (' + (p.type || p.param_type || 'string') + ')';
      }).join(', ');

      propsPanel.innerHTML =
        '<h4>Formula Metadata</h4>' +
        '<label>Name (slug)</label><input id="fe-meta-name" value="' + esc(state.formula.name || meta.name || '') + '"' + disabled + '>' +
        '<label>Display Name</label><input id="fe-meta-display" value="' + esc(state.formula.display_name || '') + '"' + disabled + '>' +
        '<label>Description</label><textarea id="fe-meta-desc" rows="2"' + disabled + '>' + esc(state.formula.description || meta.description || '') + '</textarea>' +
        '<label>Version</label><input id="fe-meta-version" value="' + esc(meta.version || '1') + '"' + disabled + '>' +
        '<label>Model Domain</label><select id="fe-meta-domain"' + disabled + '>' +
          '<option value=""' + (!meta.model_domain ? ' selected' : '') + '>auto</option>' +
          '<option value="code"' + (meta.model_domain === 'code' ? ' selected' : '') + '>code</option>' +
          '<option value="general"' + (meta.model_domain === 'general' ? ' selected' : '') + '>general</option>' +
        '</select>' +
        '<label>Parameters</label><input value="' + esc(paramStr || 'none') + '" disabled>' +
        '<p style="font-size:0.75rem;color:var(--text-muted);margin-top:0.5rem;">' +
          state.steps.length + ' step(s). Click a node to edit its properties.' +
        '</p>';

      if (!state.isReadOnly) {
        bindChange('fe-meta-name', function (v) { state.formula.name = v; state.definition.formula.name = v; markDirty(); });
        bindChange('fe-meta-display', function (v) { state.formula.display_name = v; state.definition.formula.display_name = v; markDirty(); });
        bindChange('fe-meta-desc', function (v) { state.formula.description = v; state.definition.formula.description = v; markDirty(); });
        bindChange('fe-meta-version', function (v) { state.definition.formula.version = v; markDirty(); });
        bindChange('fe-meta-domain', function (v) { state.definition.formula.model_domain = v || null; markDirty(); });
      }
      return;
    }

    // Show step properties
    var s = state.steps[state.selectedIndex];
    if (!s) return;
    var inputStr = s.input ? (typeof s.input === 'string' ? s.input : s.input.join(', ')) : '';
    var toolsStr = (s.tools || []).join(', ');

    propsPanel.innerHTML =
      '<h4>Step: ' + esc(s.name) + '</h4>' +
      '<label>Name</label><input id="fe-step-name" value="' + esc(s.name) + '"' + disabled + '>' +
      '<label>Agent Role</label><input id="fe-step-role" value="' + esc(s.agent_role) + '"' + disabled + '>' +
      '<label>Tools (comma-separated)</label><input id="fe-step-tools" value="' + esc(toolsStr) + '"' + disabled + '>' +
      '<label>Input (from step output)</label><input id="fe-step-input" value="' + esc(inputStr) + '"' + disabled + '>' +
      '<label>Output variable</label><input id="fe-step-output" value="' + esc(s.output) + '"' + disabled + '>' +
      '<label>Prompt</label><textarea id="fe-step-prompt" rows="4"' + disabled + '>' + esc(s.prompt) + '</textarea>' +
      '<label>Parallel Group</label><input id="fe-step-group" value="' + (s.group != null ? s.group : '') + '" type="number" min="0"' + disabled + '>' +
      '<label>Condition (when)</label><input id="fe-step-when" value="' + esc(s.when || '') + '"' + disabled + '>' +
      '<label>Retries</label><input id="fe-step-retries" value="' + (s.retries || '') + '" type="number" min="0"' + disabled + '>' +
      '<label>Timeout (sec)</label><input id="fe-step-timeout" value="' + (s.timeout_seconds || '') + '" type="number" min="0"' + disabled + '>' +
      '<label>System Prompt</label><textarea id="fe-step-sysprompt" rows="2"' + disabled + '>' + esc(s.system_prompt || '') + '</textarea>' +
      '<label>Model Domain</label><select id="fe-step-domain"' + disabled + '>' +
        '<option value=""' + (!s.model_domain ? ' selected' : '') + '>inherit</option>' +
        '<option value="code"' + (s.model_domain === 'code' ? ' selected' : '') + '>code</option>' +
        '<option value="general"' + (s.model_domain === 'general' ? ' selected' : '') + '>general</option>' +
      '</select>' +
      (state.isReadOnly ? '' :
        '<div class="fe-props-actions">' +
          '<button class="btn btn-sm" onclick="FormulaEditor.deleteStep(' + state.selectedIndex + ')">Delete Step</button>' +
        '</div>');

    if (!state.isReadOnly) {
      bindChange('fe-step-name', function (v) { s.name = v; markDirty(); render(); });
      bindChange('fe-step-role', function (v) { s.agent_role = v; markDirty(); render(); });
      bindChange('fe-step-tools', function (v) { s.tools = v ? v.split(',').map(function (t) { return t.trim(); }).filter(Boolean) : []; markDirty(); });
      bindChange('fe-step-input', function (v) {
        if (!v) { s.input = null; }
        else if (v.indexOf(',') >= 0) { s.input = v.split(',').map(function (t) { return t.trim(); }).filter(Boolean); }
        else { s.input = v.trim(); }
        markDirty(); render();
      });
      bindChange('fe-step-output', function (v) { s.output = v; markDirty(); render(); });
      bindChange('fe-step-prompt', function (v) { s.prompt = v; markDirty(); });
      bindChange('fe-step-group', function (v) { s.group = v !== '' ? parseInt(v, 10) : null; markDirty(); computeLayout(); render(); });
      bindChange('fe-step-when', function (v) { s.when = v || null; markDirty(); });
      bindChange('fe-step-retries', function (v) { s.retries = v ? parseInt(v, 10) : null; markDirty(); });
      bindChange('fe-step-timeout', function (v) { s.timeout_seconds = v ? parseInt(v, 10) : null; markDirty(); });
      bindChange('fe-step-sysprompt', function (v) { s.system_prompt = v || null; markDirty(); });
      bindChange('fe-step-domain', function (v) { s.model_domain = v || null; markDirty(); });
    }
  }

  function bindChange(id, fn) {
    var el = document.getElementById(id);
    if (!el) return;
    el.addEventListener('input', function () { fn(el.value); validate(); });
  }

  // ── Validation ────────────────────────────────────────────────────

  var validateTimer = null;
  function validate() {
    clearTimeout(validateTimer);
    validateTimer = setTimeout(doValidate, 300);
  }

  function doValidate() {
    state.errors = [];
    var names = {};

    state.steps.forEach(function (s) {
      // Unique names
      if (names[s.name]) {
        state.errors.push({ step: s.name, msg: 'Duplicate step name: ' + s.name });
      }
      names[s.name] = true;

      // Non-empty prompt
      if (!s.prompt || !s.prompt.trim()) {
        state.errors.push({ step: s.name, msg: s.name + ': prompt is empty' });
      }

      // Non-empty output
      if (!s.output || !s.output.trim()) {
        state.errors.push({ step: s.name, msg: s.name + ': output is empty' });
      }

      // Valid input references
      if (s.input) {
        var inputs = typeof s.input === 'string' ? [s.input] : s.input;
        inputs.forEach(function (inp) {
          var found = state.steps.some(function (o) { return o.output === inp; });
          if (!found) {
            state.errors.push({ step: s.name, msg: s.name + ': input "' + inp + '" references unknown output' });
          }
        });
      }
    });

    // Update validation bar
    if (validationBar) {
      if (state.errors.length === 0) {
        validationBar.className = 'fe-validation valid';
        validationBar.textContent = state.steps.length + ' steps, no errors';
      } else {
        validationBar.className = 'fe-validation has-errors';
        validationBar.textContent = state.errors.length + ' error(s): ' + state.errors[0].msg;
      }
    }

    render();
  }

  // ── Interaction: Node drag ────────────────────────────────────────

  function onSvgMouseDown(e) {
    var nodeG = findNodeGroup(e.target);
    if (nodeG) {
      var idx = parseInt(nodeG.getAttribute('data-index'), 10);
      state.selectedIndex = idx;
      updateProps();
      render();

      if (!state.isReadOnly) {
        var s = state.steps[idx];
        var pt = svgPoint(e);
        state.dragState = { index: idx, startX: pt.x, startY: pt.y, offsetX: s._x, offsetY: s._y };
      }
      e.preventDefault();
    } else {
      // Click on empty space — deselect
      state.selectedIndex = -1;
      updateProps();
      render();
    }
  }

  function onSvgMouseMove(e) {
    if (!state.dragState) return;
    var pt = svgPoint(e);
    var dx = pt.x - state.dragState.startX;
    var dy = pt.y - state.dragState.startY;
    var s = state.steps[state.dragState.index];
    s._x = state.dragState.offsetX + dx;
    s._y = state.dragState.offsetY + dy;
    render();
  }

  function onSvgMouseUp() {
    state.dragState = null;
  }

  function findNodeGroup(el) {
    while (el && el !== svg) {
      if (el.classList && el.classList.contains('fe-node')) return el;
      el = el.parentElement;
    }
    return null;
  }

  function svgPoint(e) {
    var rect = svg.getBoundingClientRect();
    return { x: e.clientX - rect.left - state.panX, y: e.clientY - rect.top - state.panY };
  }

  // ── Keyboard ──────────────────────────────────────────────────────

  function onKeyDown(e) {
    if (!overlay || !overlay.classList.contains('active')) return;
    // Don't intercept input fields
    if (e.target.tagName === 'INPUT' || e.target.tagName === 'TEXTAREA' || e.target.tagName === 'SELECT') return;

    if (e.key === 'Escape') {
      if (state.selectedIndex >= 0) {
        state.selectedIndex = -1;
        updateProps();
        render();
      } else {
        close();
      }
      e.preventDefault();
    } else if (e.key === 'Delete' || e.key === 'Backspace') {
      if (state.selectedIndex >= 0 && !state.isReadOnly) {
        deleteStep(state.selectedIndex);
        e.preventDefault();
      }
    } else if (e.key === 'Tab') {
      e.preventDefault();
      var dir = e.shiftKey ? -1 : 1;
      state.selectedIndex = (state.selectedIndex + dir + state.steps.length) % state.steps.length;
      updateProps();
      render();
    }
  }

  // ── Step operations ───────────────────────────────────────────────

  function addStep() {
    if (state.isReadOnly) return;
    var n = state.steps.length + 1;
    var newStep = {
      name: 'step_' + n,
      agent_role: 'worker',
      tools: [],
      prompt: '',
      output: 'output_' + n,
      input: null,
      _x: 0, _y: 0, _index: state.steps.length,
    };
    // Auto-wire: if there's a previous step, set input to its output
    if (state.steps.length > 0) {
      newStep.input = state.steps[state.steps.length - 1].output;
    }
    state.steps.push(newStep);
    state.selectedIndex = state.steps.length - 1;
    markDirty();
    computeLayout();
    render();
    updateProps();
    validate();
  }

  function deleteStep(index) {
    if (state.isReadOnly || state.steps.length <= 1) return;
    state.steps.splice(index, 1);
    state.steps.forEach(function (s, i) { s._index = i; });
    if (state.selectedIndex >= state.steps.length) state.selectedIndex = state.steps.length - 1;
    markDirty();
    computeLayout();
    render();
    updateProps();
    validate();
  }

  // ── Save ──────────────────────────────────────────────────────────

  function save() {
    if (state.isReadOnly) return;
    doValidate();
    if (state.errors.length > 0) {
      alert('Fix validation errors before saving:\n' + state.errors.map(function (e) { return '- ' + e.msg; }).join('\n'));
      return;
    }

    var definition = serializeDefinition();

    if (state.isNew) {
      var name = state.formula.name;
      if (!name) { alert('Formula name is required.'); return; }

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
        overlay.classList.remove('active');
        if (window.Ctrl && window.Ctrl.loadFormulas) window.Ctrl.loadFormulas();
        alert('Formula "' + name + '" created.');
      }).catch(function (e) { alert('Save failed: ' + e); });
    } else {
      var payload = {
        definition: definition,
        display_name: state.formula.display_name,
        description: state.formula.description,
      };

      BL.fetchApi('/api/v1/formulas/' + encodeURIComponent(state.formula.name) + '/update', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload),
      }).then(function () {
        state.isDirty = false;
        overlay.classList.remove('active');
        if (window.Ctrl && window.Ctrl.loadFormulas) window.Ctrl.loadFormulas();
        alert('Formula "' + state.formula.name + '" updated.');
      }).catch(function (e) { alert('Update failed: ' + e); });
    }
  }

  function cloneFormula() {
    var newName = prompt('Clone as (new name):', state.formula.name + '-custom');
    if (!newName) return;
    state.formula.name = newName;
    state.formula.display_name = newName;
    state.formula.is_system = false;
    state.definition.formula.name = newName;
    state.isReadOnly = false;
    state.isNew = true;
    state.isDirty = true;
    if (titleEl) titleEl.textContent = newName;
    updateProps();
    render();
  }

  // ── Helpers ───────────────────────────────────────────────────────

  function markDirty() { state.isDirty = true; }

  function esc(s) { return BL.escapeHtml(s || ''); }

  function truncate(s, n) { return s && s.length > n ? s.substring(0, n - 1) + '\u2026' : (s || ''); }

  function svgEl(tag, attrs) {
    var el = document.createElementNS(NS, tag);
    if (attrs) Object.keys(attrs).forEach(function (k) { el.setAttribute(k, attrs[k]); });
    return el;
  }

  // ── Public API ────────────────────────────────────────────────────

  window.FormulaEditor = {
    open: open,
    openNew: openNew,
    close: close,
    addStep: addStep,
    deleteStep: deleteStep,
    save: save,
    clone: cloneFormula,
  };

  // Auto-init when DOM is ready
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})();
