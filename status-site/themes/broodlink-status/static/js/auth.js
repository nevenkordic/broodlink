/*
 * Broodlink - Multi-agent AI orchestration system
 * Copyright (C) 2025-2026 Neven Kordic <neven@broodlink.ai>
 * SPDX-License-Identifier: AGPL-3.0-or-later
 *
 * Dashboard authentication module — session-based auth with role enforcement.
 */
(function (window) {
  'use strict';

  var BL = window.Broodlink;
  if (!BL) return;

  var STORAGE_TOKEN = 'broodlink_session_token';
  var STORAGE_ROLE = 'broodlink_session_role';
  var STORAGE_EXPIRES = 'broodlink_session_expires';
  var STORAGE_USERNAME = 'broodlink_session_username';

  function getToken() {
    return sessionStorage.getItem(STORAGE_TOKEN);
  }

  function getRole() {
    return sessionStorage.getItem(STORAGE_ROLE) || 'viewer';
  }

  function getUsername() {
    return sessionStorage.getItem(STORAGE_USERNAME) || '';
  }

  function isExpired() {
    var exp = sessionStorage.getItem(STORAGE_EXPIRES);
    if (!exp) return true;
    return new Date(exp).getTime() < Date.now();
  }

  function isAuthenticated() {
    return !!getToken() && !isExpired();
  }

  function canWrite() {
    var r = getRole();
    return r === 'operator' || r === 'admin';
  }

  function isAdmin() {
    return getRole() === 'admin';
  }

  function login(username, password) {
    return fetch(BL.STATUS_API_URL + '/api/v1/auth/login', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username: username, password: password })
    })
    .then(function (res) {
      if (!res.ok) {
        return res.json().then(function (e) {
          throw new Error(e.error || 'Login failed');
        });
      }
      return res.json();
    })
    .then(function (json) {
      var data = json.data || json;
      sessionStorage.setItem(STORAGE_TOKEN, data.token);
      sessionStorage.setItem(STORAGE_ROLE, data.role);
      sessionStorage.setItem(STORAGE_EXPIRES, data.expires_at);
      sessionStorage.setItem(STORAGE_USERNAME, username);
      return data;
    });
  }

  function logout() {
    var token = getToken();
    var headers = { 'Content-Type': 'application/json' };
    if (token) headers['X-Broodlink-Session'] = token;

    return fetch(BL.STATUS_API_URL + '/api/v1/auth/logout', {
      method: 'POST',
      headers: headers
    })
    .catch(function () { /* ignore errors on logout */ })
    .then(function () {
      sessionStorage.removeItem(STORAGE_TOKEN);
      sessionStorage.removeItem(STORAGE_ROLE);
      sessionStorage.removeItem(STORAGE_EXPIRES);
      sessionStorage.removeItem(STORAGE_USERNAME);
      window.location.href = '/login/';
    });
  }

  function requireAuth() {
    // Skip auth check on login page
    if (window.location.pathname === '/login/' || window.location.pathname === '/login') {
      return;
    }
    if (!isAuthenticated()) {
      window.location.href = '/login/';
    }
  }

  function injectRoleBadge() {
    var badge = document.getElementById('auth-role-badge');
    if (!badge) return;
    if (isAuthenticated()) {
      badge.innerHTML =
        '<span class="auth-badge auth-badge-' + BL.escapeHtml(getRole()) + '">' +
        BL.escapeHtml(getUsername()) + ' (' + BL.escapeHtml(getRole()) + ')' +
        '</span> ' +
        '<button class="btn btn-sm btn-ghost" onclick="BLAuth.logout()">Logout</button>';
    }
  }

  // Exports
  window.BLAuth = {
    getToken: getToken,
    getRole: getRole,
    getUsername: getUsername,
    isAuthenticated: isAuthenticated,
    canWrite: canWrite,
    isAdmin: isAdmin,
    login: login,
    logout: logout,
    requireAuth: requireAuth,
    injectRoleBadge: injectRoleBadge
  };

})(window);
