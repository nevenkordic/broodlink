/*
 * Broodlink - Multi-agent AI orchestration system
 * Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
 * SPDX-License-Identifier: AGPL-3.0-or-later
 */

(function () {
  'use strict';

  var BL = window.Broodlink;

  // ── Native Chat UI ──────────────────────────────────────────────────
  var chatMessages = document.getElementById('chat-messages');
  var chatInput = document.getElementById('chat-input');
  var chatSendBtn = document.getElementById('chat-send-btn');
  var chatNewBtn = document.getElementById('chat-new-btn');
  var chatModelSelect = document.getElementById('chat-model');
  var chatHistoryList = document.getElementById('chat-history-list');
  var chatEmpty = document.getElementById('chat-empty');

  var STORAGE_KEY = 'broodlink_chat_history';
  var conversations = loadConversations();
  var activeConversationId = null;
  var isGenerating = false;

  function loadConversations() {
    try {
      return JSON.parse(localStorage.getItem(STORAGE_KEY)) || {};
    } catch (e) {
      return {};
    }
  }

  function saveConversations() {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(conversations));
    } catch (e) { /* quota exceeded — silently ignore */ }
  }

  function generateId() {
    return Date.now().toString(36) + Math.random().toString(36).slice(2, 8);
  }

  // Load available models from Ollama
  function loadModels() {
    if (!chatModelSelect) return;
    fetch('/api/ollama/api/tags')
      .then(function (res) { return res.json(); })
      .then(function (data) {
        var models = (data.models || []).sort(function (a, b) {
          return a.name.localeCompare(b.name);
        });
        chatModelSelect.innerHTML = '';
        if (models.length === 0) {
          chatModelSelect.innerHTML = '<option value="">No models found</option>';
          return;
        }
        var savedModel = localStorage.getItem('broodlink_chat_model') || '';
        models.forEach(function (m) {
          var opt = document.createElement('option');
          opt.value = m.name;
          opt.textContent = m.name;
          if (m.name === savedModel) opt.selected = true;
          chatModelSelect.appendChild(opt);
        });
        // If no saved model matched, first option is already selected
      })
      .catch(function () {
        chatModelSelect.innerHTML = '<option value="">Ollama unavailable</option>';
      });
  }

  if (chatModelSelect) {
    chatModelSelect.addEventListener('change', function () {
      localStorage.setItem('broodlink_chat_model', chatModelSelect.value);
    });
  }

  // Render conversation history sidebar
  function renderHistory() {
    if (!chatHistoryList) return;
    var ids = Object.keys(conversations).sort(function (a, b) {
      var ta = conversations[a].updatedAt || 0;
      var tb = conversations[b].updatedAt || 0;
      return tb - ta;
    });
    if (ids.length === 0) {
      chatHistoryList.innerHTML = '<p style="opacity:0.5;font-size:0.8rem;padding:0.5rem;">No conversations yet.</p>';
      return;
    }
    chatHistoryList.innerHTML = ids.map(function (id) {
      var c = conversations[id];
      var title = BL.escapeHtml(c.title || 'Untitled');
      var active = id === activeConversationId ? ' active' : '';
      return '<div class="chat-history-item' + active + '" data-id="' + id + '">' +
        '<span class="chat-history-title">' + title + '</span>' +
        '<button class="chat-history-delete" data-id="' + id + '" title="Delete">&times;</button>' +
      '</div>';
    }).join('');

    // Click handlers
    chatHistoryList.querySelectorAll('.chat-history-item').forEach(function (el) {
      el.addEventListener('click', function (e) {
        if (e.target.classList.contains('chat-history-delete')) return;
        switchConversation(el.getAttribute('data-id'));
      });
    });
    chatHistoryList.querySelectorAll('.chat-history-delete').forEach(function (btn) {
      btn.addEventListener('click', function (e) {
        e.stopPropagation();
        var id = btn.getAttribute('data-id');
        delete conversations[id];
        saveConversations();
        if (id === activeConversationId) {
          activeConversationId = null;
          renderMessages();
        }
        renderHistory();
      });
    });
  }

  function switchConversation(id) {
    activeConversationId = id;
    renderMessages();
    renderHistory();
  }

  function renderMessages() {
    if (!chatMessages) return;
    if (!activeConversationId || !conversations[activeConversationId]) {
      chatMessages.innerHTML = '<div class="chat-empty-state" id="chat-empty">' +
        '<p>Start a conversation with your AI models.</p>' +
        '<p style="opacity:0.6;font-size:0.85rem;">Messages are processed locally via Ollama.</p>' +
      '</div>';
      return;
    }
    var msgs = conversations[activeConversationId].messages || [];
    if (msgs.length === 0) {
      chatMessages.innerHTML = '<div class="chat-empty-state">' +
        '<p>Send a message to begin.</p>' +
      '</div>';
      return;
    }
    chatMessages.innerHTML = msgs.map(function (m) {
      var cls = m.role === 'user' ? 'user' : 'assistant';
      return '<div class="chat-bubble ' + cls + '">' +
        '<div class="chat-bubble-content">' + formatMessageContent(m.content) + '</div>' +
      '</div>';
    }).join('');
    chatMessages.scrollTop = chatMessages.scrollHeight;
  }

  function formatMessageContent(text) {
    if (!text) return '';
    // Escape HTML, then convert code blocks and newlines
    var escaped = BL.escapeHtml(text);
    // Fenced code blocks
    escaped = escaped.replace(/```(\w*)\n?([\s\S]*?)```/g, function (_, lang, code) {
      return '<pre><code>' + code.trim() + '</code></pre>';
    });
    // Inline code
    escaped = escaped.replace(/`([^`]+)`/g, '<code>$1</code>');
    // Bold
    escaped = escaped.replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>');
    // Newlines
    escaped = escaped.replace(/\n/g, '<br>');
    return escaped;
  }

  function newConversation() {
    activeConversationId = generateId();
    conversations[activeConversationId] = {
      title: 'New Chat',
      messages: [],
      model: chatModelSelect ? chatModelSelect.value : '',
      createdAt: Date.now(),
      updatedAt: Date.now()
    };
    saveConversations();
    renderHistory();
    renderMessages();
    if (chatInput) chatInput.focus();
  }

  function sendMessage() {
    if (isGenerating) return;
    var text = chatInput ? chatInput.value.trim() : '';
    if (!text) return;

    var model = chatModelSelect ? chatModelSelect.value : '';
    if (!model) {
      alert('No model selected. Please select a model or ensure Ollama is running.');
      return;
    }

    // Create conversation if none active
    if (!activeConversationId || !conversations[activeConversationId]) {
      newConversation();
    }

    var conv = conversations[activeConversationId];
    conv.messages.push({ role: 'user', content: text });

    // Set title from first message
    if (conv.messages.length === 1) {
      conv.title = text.length > 40 ? text.substring(0, 40) + '…' : text;
    }
    conv.model = model;
    conv.updatedAt = Date.now();
    saveConversations();

    chatInput.value = '';
    chatInput.style.height = 'auto';
    renderMessages();
    renderHistory();

    // Show typing indicator
    isGenerating = true;
    updateSendButton();
    var typingEl = document.createElement('div');
    typingEl.className = 'chat-bubble assistant typing';
    typingEl.innerHTML = '<div class="chat-bubble-content"><span class="chat-typing-dots"><span>.</span><span>.</span><span>.</span></span></div>';
    chatMessages.appendChild(typingEl);
    chatMessages.scrollTop = chatMessages.scrollHeight;

    // Build messages array for Ollama
    var ollamaMessages = conv.messages.map(function (m) {
      return { role: m.role, content: m.content };
    });

    fetch('/api/ollama/api/chat', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        model: model,
        messages: ollamaMessages,
        stream: false
      })
    })
    .then(function (res) {
      if (!res.ok) throw new Error('Ollama returned ' + res.status);
      return res.json();
    })
    .then(function (data) {
      var content = (data.message && data.message.content) || '';
      conv.messages.push({ role: 'assistant', content: content });
      conv.updatedAt = Date.now();
      saveConversations();
      renderMessages();
    })
    .catch(function (err) {
      conv.messages.push({ role: 'assistant', content: 'Error: ' + err.message });
      conv.updatedAt = Date.now();
      saveConversations();
      renderMessages();
    })
    .finally(function () {
      isGenerating = false;
      updateSendButton();
    });
  }

  function updateSendButton() {
    if (!chatSendBtn) return;
    chatSendBtn.disabled = isGenerating;
    chatSendBtn.textContent = isGenerating ? '...' : 'Send';
  }

  // Auto-resize textarea
  if (chatInput) {
    chatInput.addEventListener('input', function () {
      this.style.height = 'auto';
      this.style.height = Math.min(this.scrollHeight, 150) + 'px';
    });
    chatInput.addEventListener('keydown', function (e) {
      if (e.key === 'Enter' && !e.shiftKey) {
        e.preventDefault();
        sendMessage();
      }
    });
  }

  if (chatSendBtn) chatSendBtn.addEventListener('click', sendMessage);
  if (chatNewBtn) chatNewBtn.addEventListener('click', newConversation);

  // Initialize chat UI
  loadModels();
  renderHistory();
  renderMessages();

  // If no conversations exist, show empty state
  if (Object.keys(conversations).length > 0) {
    // Auto-select most recent conversation
    var sorted = Object.keys(conversations).sort(function (a, b) {
      return (conversations[b].updatedAt || 0) - (conversations[a].updatedAt || 0);
    });
    switchConversation(sorted[0]);
  }

  // ── Agent Conversation Monitoring ───────────────────────────────────
  var sessionsEl = document.getElementById('chat-sessions');
  if (!sessionsEl) return;

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

  function formatFileSize(bytes) {
    if (!bytes) return '';
    if (bytes < 1024) return bytes + ' B';
    if (bytes < 1048576) return (bytes / 1024).toFixed(1) + ' KB';
    return (bytes / 1048576).toFixed(1) + ' MB';
  }

  function renderAttachments(attachments) {
    if (!attachments || attachments.length === 0) return '';
    var items = attachments.map(function (att) {
      var icon = '📎';
      if (att.attachment_type === 'image') icon = '🖼️';
      else if (att.attachment_type === 'document') icon = '📄';
      else if (att.attachment_type === 'voice' || att.attachment_type === 'audio') icon = '🎤';
      var name = BL.escapeHtml(att.file_name || 'file');
      var size = formatFileSize(att.file_size_bytes);
      var thumb = '';
      if (att.attachment_type === 'image' && att.thumbnail_path) {
        thumb = '<img class="chat-thumbnail" src="/api/v1/chat/attachments/' + BL.escapeHtml(att.id) + '/thumbnail" alt="thumbnail" loading="lazy">';
      }
      var transcription = '';
      if (att.transcription) {
        transcription = '<div class="chat-transcription"><em>Transcription:</em> ' + BL.escapeHtml(att.transcription) + '</div>';
      }
      if (att.extracted_text) {
        transcription += '<div class="chat-transcription"><em>Extracted text:</em> ' + BL.escapeHtml(att.extracted_text.substring(0, 500)) + (att.extracted_text.length > 500 ? '…' : '') + '</div>';
      }
      return '<div class="chat-attachment chat-attachment-' + BL.escapeHtml(att.attachment_type) + '">' +
        thumb +
        '<div class="chat-attachment-info">' +
          '<span class="chat-attachment-icon">' + icon + '</span>' +
          '<a href="/api/v1/chat/attachments/' + BL.escapeHtml(att.id) + '/download" class="chat-attachment-name" target="_blank">' + name + '</a>' +
          (size ? '<span class="chat-attachment-size">' + size + '</span>' : '') +
        '</div>' +
        transcription +
      '</div>';
    }).join('');
    return '<div class="chat-attachments">' + items + '</div>';
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
        var attHtml = renderAttachments(m.attachments || []);
        return '<div class="chat-msg ' + cls + '">' +
          '<div class="chat-msg-header"><strong>' + label + '</strong> <span class="time">' + time + '</span></div>' +
          '<div class="chat-msg-content">' + BL.escapeHtml(m.content) + '</div>' +
          attHtml +
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

  // Initial load + auto-refresh for agent conversations
  loadStats();
  loadSessions();
  setInterval(function () { loadStats(); loadSessions(); }, BL.REFRESH_INTERVAL || 10000);
})();
