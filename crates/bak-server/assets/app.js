// bak-server dashboard. Plain DOM, no dependencies; all text goes through
// textContent, never innerHTML.
"use strict";

const state = { me: null, timer: null };
const $ = (sel) => document.querySelector(sel);

function h(tag, attrs, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v === false || v === null || v === undefined) continue;
    if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
    else if (k === "class") el.className = v;
    else el.setAttribute(k, v === true ? "" : v);
  }
  for (const c of children.flat()) {
    if (c === null || c === undefined || c === false) continue;
    el.append(c instanceof Node ? c : document.createTextNode(String(c)));
  }
  return el;
}

async function api(method, path, body) {
  const res = await fetch("/_/api/" + path, {
    method,
    credentials: "same-origin",
    headers: { "X-Bak": "1", "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  let data = {};
  try { data = await res.json(); } catch (_) { /* empty body */ }
  if (res.status === 401 && path !== "login") { showLogin(); throw new Error("Please sign in again."); }
  if (!res.ok) throw new Error(data.error || "Request failed (" + res.status + ")");
  return data;
}

// ---------- formatting ----------
function bytes(n) {
  if (n === null || n === undefined) return "–";
  const u = ["B", "KiB", "MiB", "GiB", "TiB"];
  let i = 0;
  while (n >= 1024 && i < u.length - 1) { n /= 1024; i++; }
  return (i === 0 ? n : n.toFixed(1)) + " " + u[i];
}
function ago(ts) {
  if (!ts) return "never";
  const s = Math.max(0, Date.now() / 1000 - ts);
  if (s < 60) return "just now";
  if (s < 3600) return Math.floor(s / 60) + " min ago";
  if (s < 86400) return Math.floor(s / 3600) + " h ago";
  return Math.floor(s / 86400) + " days ago";
}
function when(ts) { return ts ? new Date(ts * 1000).toLocaleString() : "–"; }
function duration(s) {
  const d = Math.floor(s / 86400), hr = Math.floor((s % 86400) / 3600), m = Math.floor((s % 3600) / 60);
  return d ? d + "d " + hr + "h" : hr ? hr + "h " + m + "m" : m + "m";
}
function freshness(ts) {
  if (!ts) return h("span", { class: "badge" }, "no backups");
  const days = (Date.now() / 1000 - ts) / 86400;
  if (days < 2) return h("span", { class: "badge ok" }, "fresh");
  if (days < 8) return h("span", { class: "badge warn" }, days.toFixed(0) + " days old");
  return h("span", { class: "badge bad" }, "stale");
}
const EVENT_LABELS = {
  backup: "Backup", create_repo: "Repository created", delete: "Deleted", refused: "Refused",
  login: "Signed in", login_failed: "Failed sign-in", lockout: "IP locked out",
  user_added: "User added", user_removed: "User removed", password_changed: "Password changed",
  admin_changed: "Role changed",
};
function eventBadge(kind) {
  const cls = { login_failed: "bad", lockout: "bad", refused: "warn", delete: "warn", backup: "ok" }[kind] || "";
  return h("span", { class: "badge " + cls }, EVENT_LABELS[kind] || kind);
}

function toast(msg) {
  const t = h("div", { class: "toast", role: "status" }, msg);
  document.body.append(t);
  setTimeout(() => t.remove(), 2500);
}
function modal(title, ...body) {
  $("#modal-title").textContent = title;
  $("#modal-body").replaceChildren(...body);
  $("#modal").hidden = false;
  $("#modal-close").focus();
}
$("#modal-close")?.addEventListener("click", () => { $("#modal").hidden = true; });

function showSecret(title, user, password) {
  const code = h("code", {}, password);
  modal(title,
    h("p", {}, "The password for ", h("strong", {}, user), " is shown only once. Copy it now."),
    h("div", { class: "secret" }, code,
      h("button", { onclick: async () => { await navigator.clipboard.writeText(password); toast("Copied"); } }, "Copy")));
}

function table(headers, rows, emptyText) {
  if (!rows.length) return h("div", { class: "empty" }, emptyText);
  return h("div", { class: "table-wrap" },
    h("table", {},
      h("thead", {}, h("tr", {}, headers.map(([label, cls]) => h("th", { class: cls || "" }, label)))),
      h("tbody", {}, rows)));
}
function stat(label, value, sub) {
  return h("div", { class: "card stat" },
    h("div", { class: "label" }, label), h("div", { class: "value" }, value), sub ? h("div", { class: "sub" }, sub) : null);
}

// ---------- views ----------
const views = {
  async overview(main) {
    const [o, events] = await Promise.all([api("GET", "overview"), api("GET", "activity")]);
    const admin = state.me.admin;
    main.replaceChildren(
      h("div", { class: "head" },
        h("div", {}, h("h1", {}, "Overview"), h("p", { class: "muted" }, "bak-server " + o.version + " · up " + duration(o.uptime_secs))),
        h("div", { class: "badges" },
          h("span", { class: "badge " + (o.tls ? "ok" : "bad") }, o.tls ? "HTTPS" : "plain HTTP"),
          h("span", { class: "badge " + (o.append_only ? "ok" : "warn") }, o.append_only ? "append-only" : "deletes allowed"),
          h("span", { class: "badge" }, o.private_repos ? "private repositories" : "shared repositories"))),
      h("div", { class: "stats" },
        stat("Repositories", o.repos),
        stat("Stored", bytes(o.total_bytes)),
        stat("Snapshots", o.snapshots),
        stat("Last backup", ago(o.last_backup), when(o.last_backup)),
        stat("Backups (24 h)", o.backups_24h),
        admin ? stat("Users", o.users) : null,
        admin ? stat("Failed sign-ins (24 h)", o.failed_logins_24h, o.locked_ips ? o.locked_ips + " IP(s) locked out" : "no lockouts") : null,
        admin ? stat("Traffic", bytes(o.bytes_in) + " in", bytes(o.bytes_out) + " out · " + o.requests + " requests") : null),
      h("div", { class: "card section" }, h("h2", {}, "Recent activity"),
        activityTable(events.slice(0, 8))),
      admin ? h("p", { class: "muted" }, "Data directory: ", h("code", {}, o.data_path)) : null);
  },

  async repos(main) {
    const repos = await api("GET", "repos");
    const rows = repos.map((r) => h("tr", {},
      h("td", {}, h("code", {}, r.path)),
      state.me.admin ? h("td", {}, r.owner || "–") : null,
      h("td", { class: "num" }, bytes(r.bytes)),
      h("td", { class: "num" }, r.snapshots),
      h("td", { class: "num" }, r.packs),
      h("td", { title: when(r.last_backup) }, ago(r.last_backup)),
      h("td", {}, freshness(r.last_backup)),
      h("td", { title: when(r.created) }, r.created ? new Date(r.created * 1000).toLocaleDateString() : "–")));
    main.replaceChildren(
      h("div", { class: "head" }, h("div", {}, h("h1", {}, "Repositories"),
        h("p", { class: "muted" }, "Contents are encrypted; the server sees only sizes and times. Repositories are never deleted from here."))),
      h("div", { class: "card" }, table(
        [["Path"], ...(state.me.admin ? [["Owner"]] : []), ["Size", "num"], ["Snapshots", "num"], ["Packs", "num"], ["Last backup"], ["Status"], ["Created"]],
        rows, "No repositories yet. Connect a client to create one.")));
  },

  async users(main) {
    if (!state.me.admin) { location.hash = "#/overview"; return; }
    const users = await api("GET", "users");
    const rows = users.map((u) => h("tr", {},
      h("td", {}, h("strong", {}, u.name), u.name === state.me.user ? h("span", { class: "muted" }, " (you)") : null),
      h("td", {}, u.admin ? h("span", { class: "badge ok" }, "admin") : h("span", { class: "badge" }, "user")),
      h("td", { class: "num" }, u.repos),
      h("td", {}, u.last_seen ? ago(u.last_seen) : "not since restart"),
      h("td", {}, h("div", { class: "row-actions" },
        h("button", { onclick: () => resetPassword(u.name) }, "Reset password"),
        u.name !== state.me.user ? h("button", { onclick: () => toggleAdmin(u) }, u.admin ? "Make user" : "Make admin") : null,
        u.name !== state.me.user ? h("button", { class: "danger", onclick: () => removeUser(u.name) }, "Remove") : null))));

    const form = h("form", { class: "card section" },
      h("h2", {}, "Add a user"),
      h("div", { class: "form-row" },
        h("label", {}, "User name", h("input", { name: "name", required: true, pattern: "[A-Za-z0-9][A-Za-z0-9._-]*", autocomplete: "off" })),
        h("label", {}, "Password", h("input", { name: "password", type: "password", minlength: "12", placeholder: "leave empty to generate", autocomplete: "new-password" })),
        h("label", { class: "check" }, h("input", { name: "admin", type: "checkbox" }), "Administrator")),
      h("p", { class: "error", "data-error": true }),
      h("div", { class: "actions" }, h("button", { class: "primary", type: "submit" }, "Add user")));
    form.addEventListener("submit", async (e) => {
      e.preventDefault();
      const f = new FormData(form);
      const password = f.get("password");
      try {
        const res = await api("POST", "users", {
          name: f.get("name"), admin: f.get("admin") === "on",
          generate: !password, password: password || undefined,
        });
        form.reset();
        if (res.password) showSecret("User created", res.name, res.password); else toast("User " + res.name + " created");
        render();
      } catch (err) { form.querySelector("[data-error]").textContent = err.message; }
    });

    main.replaceChildren(
      h("div", { class: "head" }, h("div", {}, h("h1", {}, "Users"),
        h("p", { class: "muted" }, "Accounts used by backup clients and to sign in here. Removing a user keeps their repositories."))),
      form,
      h("div", { class: "card" }, table([["Name"], ["Role"], ["Repositories", "num"], ["Last seen"], [""]], rows, "No users.")));
  },

  async activity(main) {
    const events = await api("GET", "activity");
    const select = h("select", { "aria-label": "Filter events" },
      h("option", { value: "" }, "All events"),
      Object.entries(EVENT_LABELS).map(([k, v]) => h("option", { value: k }, v)));
    const holder = h("div", { class: "card" });
    const draw = () => holder.replaceChildren(activityTable(events.filter((e) => !select.value || e.kind === select.value)));
    select.addEventListener("change", draw);
    draw();
    main.replaceChildren(
      h("div", { class: "head" },
        h("div", {}, h("h1", {}, "Activity"), h("p", { class: "muted" }, "Most recent first. Kept in memory since the server started.")),
        select),
      holder);
  },

  async setup(main) {
    const o = await api("GET", "overview");
    const host = location.host;
    const scheme = o.tls ? "https" : "http";
    const repo = o.private_repos ? "/" + state.me.user + "/myproj" : "/myproj";
    const url = "rest:" + scheme + "://" + host + repo;
    const cacert = o.ca_available ? " --cacert ~/.config/bak/bak-server-ca.pem" : "";
    main.replaceChildren(
      h("div", { class: "head" }, h("div", {}, h("h1", {}, "Connect a client"),
        h("p", { class: "muted" }, "Back up a project from your computer to this server."))),
      o.ca_available ? h("div", { class: "card section" },
        h("h2", {}, "1. Trust this server's certificate"),
        h("p", {}, "This server uses its own certificate authority. Download it once and keep it with your bak config."),
        h("p", {}, h("a", { href: "/_/ca.pem", download: "bak-server-ca.pem" }, "Download bak-server-ca.pem")),
        h("pre", {}, "mkdir -p ~/.config/bak\nmv ~/Downloads/bak-server-ca.pem ~/.config/bak/")) : null,
      h("div", { class: "card section" },
        h("h2", {}, (o.ca_available ? "2" : "1") + ". Add this server as a remote"),
        h("p", {}, "Run this inside your project (after ", h("code", {}, "bak init"), "). Change ", h("code", {}, "myproj"), " to the project's name."),
        h("pre", {},
          "export BAK_REST_USER=" + state.me.user + "\n" +
          "export BAK_REST_PASSWORD='<your password>'   # or use --credentials-file\n" +
          "bak remote add home " + url + cacert + "\n" +
          "bak push home -m \"first backup\"")),
      h("div", { class: "card section" },
        h("h2", {}, "restic works too"),
        h("pre", {}, "restic -r " + scheme + "://" + state.me.user + ":<password>@" + host + repo.replace(/^/, "") +
          (o.ca_available ? " --cacert ~/.config/bak/bak-server-ca.pem" : "") + " snapshots")));
  },

  async account(main) {
    const form = h("form", { class: "card section" },
      h("h2", {}, "Change your password"),
      h("p", { class: "muted" }, "Backup clients using the old password will need the new one. Other dashboard sessions are signed out."),
      h("div", { class: "form-row" },
        h("label", {}, "Current password", h("input", { name: "current", type: "password", required: true, autocomplete: "current-password" })),
        h("label", {}, "New password", h("input", { name: "password", type: "password", minlength: "12", placeholder: "leave empty to generate", autocomplete: "new-password" }))),
      h("p", { class: "error", "data-error": true }),
      h("div", { class: "actions" }, h("button", { class: "primary", type: "submit" }, "Change password")));
    form.addEventListener("submit", async (e) => {
      e.preventDefault();
      const f = new FormData(form);
      const password = f.get("password");
      try {
        const res = await api("POST", "users/" + encodeURIComponent(state.me.user) + "/password", {
          current_password: f.get("current"), generate: !password, password: password || undefined,
        });
        form.reset();
        if (res.password) showSecret("New password", state.me.user, res.password); else toast("Password changed");
      } catch (err) { form.querySelector("[data-error]").textContent = err.message; }
    });
    main.replaceChildren(
      h("div", { class: "head" }, h("div", {}, h("h1", {}, "Account"),
        h("p", { class: "muted" }, "Signed in as ", h("strong", {}, state.me.user), state.me.admin ? " (administrator)" : ""))),
      form);
  },
};

function activityTable(events) {
  return table([["Time"], ["Event"], ["User"], ["Address"], ["Details"]],
    events.map((e) => h("tr", {},
      h("td", { title: when(e.time) }, ago(e.time)),
      h("td", {}, eventBadge(e.kind)),
      h("td", {}, e.user || "–"),
      h("td", {}, h("code", {}, e.ip)),
      h("td", {}, e.detail))),
    "Nothing yet.");
}

async function resetPassword(name) {
  if (name === state.me.user) { location.hash = "#/account"; return; }
  if (!confirm("Generate a new password for " + name + "? Their backup clients will need it.")) return;
  try {
    const res = await api("POST", "users/" + encodeURIComponent(name) + "/password", { generate: true });
    showSecret("New password", name, res.password);
  } catch (err) { toast(err.message); }
}
async function toggleAdmin(u) {
  try { await api("POST", "users/" + encodeURIComponent(u.name) + "/admin", { admin: !u.admin }); render(); }
  catch (err) { toast(err.message); }
}
async function removeUser(name) {
  const typed = prompt("Remove " + name + "? Their repositories are kept on disk.\nType the user name to confirm:");
  if (typed !== name) return;
  try { await api("DELETE", "users/" + encodeURIComponent(name)); toast("Removed " + name); render(); }
  catch (err) { toast(err.message); }
}

// ---------- shell ----------
async function render() {
  clearInterval(state.timer);
  const view = (location.hash.replace(/^#\//, "") || "overview").split("/")[0];
  const fn = views[view] || views.overview;
  for (const a of document.querySelectorAll("#nav a")) a.classList.toggle("active", a.dataset.view === view);
  const main = $("#main");
  try { await fn(main); } catch (err) {
    main.replaceChildren(h("div", { class: "card" }, h("p", { class: "error" }, err.message)));
  }
  if (view === "overview" || view === "activity") state.timer = setInterval(render, 30000);
}

function showLogin() {
  clearInterval(state.timer);
  $("#app").hidden = true;
  $("#login").hidden = false;
  $("#login-form input[name=user]").focus();
}

async function start() {
  try {
    state.me = await api("GET", "me");
  } catch (_) { showLogin(); return; }
  $("#login").hidden = true;
  $("#app").hidden = false;
  $("#who-name").textContent = state.me.user;
  $("#who-role").textContent = state.me.admin ? "admin" : "user";
  for (const a of document.querySelectorAll("#nav [data-admin]")) a.hidden = !state.me.admin;
  render();
}

$("#login-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const f = new FormData(e.target);
  const err = $("#login-error");
  err.textContent = "";
  try {
    await api("POST", "login", { user: f.get("user"), password: f.get("password") });
    e.target.reset();
    start();
  } catch (ex) { err.textContent = ex.message; }
});
$("#logout").addEventListener("click", async () => {
  try { await api("POST", "logout"); } catch (_) { /* already signed out */ }
  state.me = null;
  showLogin();
});
window.addEventListener("hashchange", () => { if (state.me) render(); });
start();
