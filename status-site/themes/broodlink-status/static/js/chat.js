/*
 * Broodlink - Multi-agent AI orchestration system
 * Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
 * SPDX-License-Identifier: AGPL-3.0-or-later
 */

(function () {
  'use strict';

  var sessionsEl = document.getElementById('chat-sessions');
  if (!sessionsEl) return;

  var BL = window.Broodlink;

  var platformFilter = document.getElementById('chat-platform-filter');
  var statusFilter = document.getElementById('chat-status-filter');
  var modal = document.getElementById('chat-message-modal');
  var modalTitle = document.getElementById('chat-modal-title');
  var messageList = document.getElementById('chat-message-list');
  var modalClose = document.getElementById('chat-modal-close');

  function platformIcon(platform) {
    switch (platform) {
      case 'slack': return '<span class="platform-badge slack" title="Slack">S</span>';
      case 'teams': return '<span class="platform-badge teams" title="Teams">T</span>';
      case 'telegram': return '<span class="platform-badge telegram" title="Telegram">TG</span>';
      default: return '<span class="platform-badge" title="' + BL.escapeHtml(platform) + '">?</span>';
    }
  }

  function renderSessions(sessions) {
    if (!sessions || sessions.length === 0) {
      sessionsEl.innerHTML = '<p class="empty-state">No chat sessions found.</p>';
      return;
    }
    sessionsEl.innerHTML = sessions.map(function (s) {
      var name = BL.escapeHtml(s.user_display_name || s.user_id || '?');
      var agent = s.assigned_agent ? BL.escapeHtml(s.assigned_agent) : '<em>auto</em>';
      var lastMsg = s.last_message_at ? BL.formatRelativeTime(s.last_message_at) : '—';
      var statusClass = s.status === 'active' ? 'ok' : 'offline';
      return '<div class="card chat-session-card" data-session-id="' + BL.escapeHtml(s.id) + '">' +
        '<div class="card-header">' +
          platformIcon(s.platform) +
          '<strong>' + name + '</strong>' +
          '<span class="badge badge-' + statusClass + '">' + BL.escapeHtml(s.status) + '</span>' +
        '</div>' +
        '<div class="card-body">' +
          '<div class="card-row"><span>Messages:</span><span>' + s.message_count + '</span></div>' +
          '<div class="card-row"><span>Agent:</span><span>' + agent + '</span></div>' +
          '<div class="card-row"><span>Last message:</span><span>' + lastMsg + '</span></div>' +
          '<div class="card-row"><span>Channel:</span><span class="mono">' + BL.escapeHtml(s.channel_id || '') + '</span></div>' +
        '</div>' +
        '<div class="card-actions">' +
          '<button class="btn btn-sm" onclick="window.ChatPage.viewMessages(\'' + BL.escapeHtml(s.id) + '\', \'' + name + '\')">Messages</button>' +
          (s.status === 'active' ?
            '<button class="btn btn-sm btn-danger" onclick="window.ChatPage.closeSession(\'' + BL.escapeHtml(s.id) + '\')">Close</button>' : '') +
        '</div>' +
      '</div>';
    }).join('');
  }

  function loadStats() {
    BL.fetchApi('/api/v1/chat/stats').then(function (data) {
      var el = document.getElementById('chat-active-sessions');
      if (el) el.textContent = data.active_sessions || 0;
      el = document.getElementById('chat-messages-today');
      if (el) el.textContent = data.messages_today || 0;
      el = document.getElementById('chat-pending-replies');
      if (el) el.textContent = data.pending_replies || 0;
      var platforms = data.platforms || {};
      el = document.getElementById('chat-platforms');
      if (el) el.textContent = Object.keys(platforms).length;
    }).catch(function () {});
  }

  function loadSessions() {
    var platform = platformFilter ? platformFilter.value : '';
    var status = statusFilter ? statusFilter.value : 'active';
    var params = '?status=' + encodeURIComponent(status);
    if (platform) params += '&platform=' + encodeURIComponent(platform);

    BL.fetchApi('/api/v1/chat/sessions' + params).then(function (data) {
      renderSessions(data.sessions || []);
    }).catch(function () {
      sessionsEl.innerHTML = '<p>Failed to load chat sessions.</p>';
    });
  }

  function viewMessages(sessionId, userName) {
    if (modalTitle) modalTitle.textContent = 'Messages — ' + userName;
    if (messageList) messageList.innerHTML = '<p>Loading...</p>';
    if (modal) modal.style.display = 'flex';

    BL.fetchApi('/api/v1/chat/sessions/' + sessionId + '/messages?limit=50').then(function (data) {
      var msgs = (data.messages || []).reverse();
      if (msgs.length === 0) {
        messageList.innerHTML = '<p>No messages yet.</p>';
        return;
      }
      messageList.innerHTML = msgs.map(function (m) {
        var cls = m.direction === 'inbound' ? 'chat-msg-inbound' : 'chat-msg-outbound';
        var label = m.direction === 'inbound' ? 'User' : 'Agent';
        var time = m.created_at ? BL.formatRelativeTime(m.created_at) : '';
        return '<div class="chat-msg ' + cls + '">' +
          '<div class="chat-msg-header"><strong>' + label + '</strong> <span class="time">' + time + '</span></div>' +
          '<div class="chat-msg-content">' + BL.escapeHtml(m.content) + '</div>' +
        '</div>';
      }).join('');
    }).catch(function () {
      messageList.innerHTML = '<p>Failed to load messages.</p>';
    });
  }

  function closeSession(sessionId) {
    BL.fetchApi('/api/v1/chat/sessions/' + sessionId + '/close', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: '{}'
    }).then(function () {
      loadSessions();
      loadStats();
    }).catch(function (e) {
      alert('Failed to close session: ' + e);
    });
  }

  // Event listeners
  if (platformFilter) platformFilter.addEventListener('change', loadSessions);
  if (statusFilter) statusFilter.addEventListener('change', loadSessions);
  if (modalClose) modalClose.addEventListener('click', function () {
    if (modal) modal.style.display = 'none';
  });
  if (modal) modal.addEventListener('click', function (e) {
    if (e.target === modal) modal.style.display = 'none';
  });

  // Public API for onclick handlers
  window.ChatPage = {
    viewMessages: viewMessages,
    closeSession: closeSession
  };

  // Initial load + auto-refresh
  loadStats();
  loadSessions();
  setInterval(function () { loadStats(); loadSessions(); }, BL.REFRESH_INTERVAL || 10000);
})();
