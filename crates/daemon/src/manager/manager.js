"use strict";

// The manager page holds no truth. It loads the lobby, follows the live feed,
// and loads the lobby again whenever the feed says to reset. Text reaches the
// page only through textContent; nothing becomes markup.

// ---------- small helpers ----------

function element(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text != null) node.textContent = String(text);
  return node;
}

function link(path, className, text) {
  const node = element("a", className, text);
  node.href = path;
  node.setAttribute("data-link", "");
  return node;
}

function button(label, className) {
  const node = element("button", className, label);
  node.type = "button";
  return node;
}

function field(label, value, hint) {
  const wrapper = element("label", "", label);
  const input = element("input");
  input.type = "text";
  input.value = value || "";
  input.autocomplete = "off";
  input.spellcheck = false;
  wrapper.append(input);
  if (hint) wrapper.append(element("span", "hint", hint));
  return { wrapper, input };
}

// Every call answers { ok, status, body }; a failure carries the lobby's own sentence.
async function call(path, method, payload) {
  const options = { method: method || "GET", headers: {} };
  if (payload !== undefined) {
    options.headers["Content-Type"] = "application/json";
    options.body = JSON.stringify(payload);
  }
  try {
    const response = await fetch(path, options);
    const body = await response.json().catch(() => ({}));
    return { ok: response.ok, status: response.status, body };
  } catch {
    return { ok: false, status: 0, body: { message: "The lobby could not be reached. Check that it runs, then reload." } };
  }
}

function sentence(result) {
  return (result.body && result.body.message) || "The lobby could not finish that. Try again.";
}

function note(message, isError) {
  const line = document.getElementById("notice");
  line.textContent = message;
  line.className = isError ? "error" : "ok";
  clearTimeout(note.timer);
  note.timer = setTimeout(() => { line.textContent = ""; }, 6000);
}

// A destructive action asks in place; the page never opens a dialog.
function confirmInPlace(trigger, question, confirmLabel, action, cancelLabel = "Keep") {
  const group = element("span", "confirm-inline");
  group.setAttribute("role", "group");
  group.setAttribute("aria-label", question);
  const yes = button(confirmLabel, "danger");
  const keep = button(cancelLabel, "quiet");
  group.append(element("span", "confirm-question", question), yes, keep);
  const restore = () => { if (group.isConnected) group.replaceWith(trigger); };
  trigger.addEventListener("click", () => { trigger.replaceWith(group); yes.focus(); });
  yes.addEventListener("click", () => { restore(); action(); });
  keep.addEventListener("click", () => { restore(); trigger.focus(); });
  group.addEventListener("keydown", event => {
    if (event.key === "Escape") { event.stopPropagation(); restore(); trigger.focus(); }
  });
}

// A moniker suggested from a display name: lowercase ASCII, single hyphens.
function monikerFrom(name) {
  return name.trim().toLowerCase().normalize("NFKD").replace(/[̀-ͯ]/g, "")
    .replace(/[^a-z0-9]+/g, "-").replace(/^[^a-z]+/, "").replace(/-+$/g, "").slice(0, 24).replace(/-+$/g, "");
}

function hue(id) {
  return Array.from(id).reduce((sum, letter) => sum + letter.charCodeAt(0), 0) % 360;
}

function avatar(companion) {
  const initial = Array.from(companion.displayName || companion.moniker || "?")[0].toUpperCase();
  const node = element("span", "avatar", initial);
  node.setAttribute("aria-hidden", "true");
  node.style.backgroundColor = "hsl(" + hue(companion.id) + " 70% 70%)";
  return node;
}

function attachedText(count) {
  if (!count) return "No mind attached";
  return count === 1 ? "1 mind attached" : count + " minds attached";
}

// ---------- the lobby: loaded, then patched by the feed ----------

const lobby = { companions: new Map(), journalHealthy: true, position: 0, loaded: false };
let resynchronizing = true;
const buffered = [];

async function loadLobby() {
  const result = await call("/api/lobby");
  if (!result.ok) return false;
  lobby.companions = new Map(result.body.companions.map(companion => [companion.id, companion]));
  lobby.position = result.body.feed.position;
  lobby.journalHealthy = result.body.journal.healthy;
  lobby.loaded = true;
  return true;
}

// Applies one feed item the loaded lobby does not include yet.
function applyItem(seq, item) {
  if (seq <= lobby.position) return false;
  lobby.position = seq;
  if (item.kind === "companion") lobby.companions.set(item.companion.id, item.companion);
  else if (item.kind === "companion_removed") lobby.companions.delete(item.id);
  else if (item.kind === "journal_health") lobby.journalHealthy = item.healthy;
  else if (item.kind === "journal") journalArrived(item.entry);
  return true;
}

async function resynchronize() {
  resynchronizing = true;
  setLive("Loading", false);
  const fresh = await loadLobby();
  resynchronizing = false;
  for (const [seq, item] of buffered.splice(0)) applyItem(seq, item);
  setLive(fresh ? "Live" : "Could not load the lobby; retrying", fresh);
  if (!fresh) setTimeout(resynchronize, 2000);
  refresh(true);
}

function setLive(text, on) {
  const live = document.getElementById("live");
  live.textContent = text;
  live.classList.toggle("on", on);
}

function startFeed() {
  if (!window.EventSource) { setLive("Live updates are not available in this browser", false); resynchronize(); return; }
  const source = new EventSource("/api/events");
  source.onmessage = message => {
    let item;
    try { item = JSON.parse(message.data); } catch { return; }
    if (item.kind === "reset") { resynchronize(); return; }
    const seq = Number(String(message.lastEventId).split(":").pop());
    if (!Number.isFinite(seq)) return;
    if (resynchronizing) buffered.push([seq, item]);
    else if (applyItem(seq, item)) refresh(false);
  };
  source.onerror = () => setLive("Reconnecting", false);
}

// ---------- the router: every state of the page is a route ----------

const view = document.getElementById("view");
let editing = false;
let refreshPending = false;
let journalView = null;

view.addEventListener("input", () => { editing = true; });

document.addEventListener("click", event => {
  const anchor = event.target.closest && event.target.closest("a[data-link]");
  if (!anchor || event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
  event.preventDefault();
  navigate(anchor.getAttribute("href"));
});
window.addEventListener("popstate", () => route());

function navigate(path) {
  history.pushState({}, "", path);
  route();
}

function route() {
  editing = false;
  refreshPending = false;
  journalView = null;
  render();
  view.focus({ preventScroll: true });
}

// A feed update waits while the sponsor edits a form, unless it must not.
function refresh(force) {
  if (editing && !force) { refreshPending = true; return; }
  editing = false;
  refreshPending = false;
  render();
}

function doneEditing() {
  editing = false;
  if (refreshPending) refresh(true);
}

function render() {
  const parts = location.pathname.split("/").filter(Boolean);
  const section = parts[0] === "journal" ? "journal" : "companions";
  document.querySelectorAll(".primary-nav [data-section]").forEach(item => {
    if (item.dataset.section === section) item.setAttribute("aria-current", "page");
    else item.removeAttribute("aria-current");
  });
  const condition = document.getElementById("condition");
  condition.hidden = lobby.journalHealthy;
  condition.textContent = lobby.journalHealthy ? "" : "The journal could not record a change. The change itself happened; the trace log in the data directory says why the journal failed.";
  if (!lobby.loaded) { view.replaceChildren(element("p", "muted", "Loading the lobby…")); return; }
  if (parts.length === 0) renderCompanions();
  else if (parts[0] === "journal" && parts.length === 1) renderJournal();
  else if (parts[0] === "companion" && parts[1] && parts.length === 2) renderCompanion(parts[1]);
  else { history.replaceState({}, "", "/"); renderCompanions(); }
}

// ---------- companions ----------

function renderCompanions() {
  const heading = element("h1", "", "Companions");
  const companions = [...lobby.companions.values()];
  let list;
  if (companions.length === 0) {
    list = element("p", "muted", "No companions yet. Create one, then give an MCP host its configuration so a mind can attach as it.");
  } else {
    list = element("ul", "cards");
    for (const companion of companions) {
      const card = element("li", "card");
      const words = element("div");
      words.append(link("/companion/" + encodeURIComponent(companion.id), "", companion.displayName),
        element("p", "", companion.moniker + " · " + attachedText(companion.attachments)));
      card.append(avatar(companion), words);
      list.append(card);
    }
  }
  view.replaceChildren(heading, list, createForm());
}

function createForm() {
  const form = element("form");
  form.setAttribute("aria-label", "Create a companion");
  const name = field("Display name", "", "What people read.");
  const moniker = field("Moniker", "", "What a host configuration may name it by: lowercase letters, digits and single hyphens, starting with a letter.");
  let monikerEdited = false;
  name.input.addEventListener("input", () => { if (!monikerEdited) moniker.input.value = monikerFrom(name.input.value); });
  moniker.input.addEventListener("input", () => { monikerEdited = true; });
  const create = element("button", "primary", "Create companion");
  create.type = "submit";
  const actions = element("div", "actions");
  actions.append(create);
  form.append(element("h2", "", "Create a companion"), name.wrapper, moniker.wrapper, actions);
  form.addEventListener("submit", async event => {
    event.preventDefault();
    create.disabled = true;
    const displayName = name.input.value.trim();
    const result = await call("/api/companions", "POST", { moniker: moniker.input.value.trim(), displayName: displayName || moniker.input.value.trim() });
    create.disabled = false;
    if (!result.ok) { note(sentence(result), true); return; }
    const companion = result.body.companion;
    note(companion.displayName + " was created.");
    navigate("/companion/" + encodeURIComponent(companion.id));
  });
  return form;
}

function renderCompanion(id) {
  const companion = lobby.companions.get(id);
  const back = link("/", "", "All companions");
  if (!companion) {
    view.replaceChildren(back, element("h1", "", "Not here"), element("p", "muted", "No companion has that id; it may have been removed."));
    return;
  }
  const heading = element("h1", "", companion.displayName);
  const facts = element("p", "muted", companion.moniker + " · " + attachedText(companion.attachments));
  view.replaceChildren(back, heading, facts, renameForm(companion), hostConfig(companion), removal(companion));
}

function renameForm(companion) {
  const form = element("form");
  form.setAttribute("aria-label", "Rename " + companion.displayName);
  const name = field("Display name", companion.displayName);
  const moniker = field("Moniker", companion.moniker);
  const warning = element("p", "warning", "A host configuration that names this companion by its moniker stops working after the moniker changes. The configuration below names it by its id and keeps working.");
  warning.hidden = true;
  moniker.input.addEventListener("input", () => { warning.hidden = moniker.input.value.trim() === companion.moniker; });
  const save = element("button", "primary", "Save");
  save.type = "submit";
  const cancel = button("Discard changes", "quiet");
  cancel.addEventListener("click", () => refresh(true));
  const actions = element("div", "actions");
  actions.append(save, cancel);
  form.append(element("h2", "", "Names"), name.wrapper, moniker.wrapper, warning, actions);
  form.addEventListener("submit", async event => {
    event.preventDefault();
    const change = { expectedRevision: companion.revision };
    if (name.input.value.trim() !== companion.displayName) change.displayName = name.input.value.trim();
    if (moniker.input.value.trim() !== companion.moniker) change.moniker = moniker.input.value.trim();
    if (!("displayName" in change) && !("moniker" in change)) { note("Nothing changed."); doneEditing(); return; }
    save.disabled = true;
    const result = await call("/api/companions/" + encodeURIComponent(companion.id), "PATCH", change);
    save.disabled = false;
    if (!result.ok) {
      note(sentence(result), true);
      if (result.status === 409) refresh(true);
      return;
    }
    lobby.companions.set(result.body.companion.id, result.body.companion);
    note("Saved.");
    refresh(true);
  });
  return form;
}

function hostConfig(companion) {
  const section = element("section");
  section.setAttribute("aria-label", "Host configuration");
  const block = element("div", "config");
  const text = element("pre", "", companion.hostConfig);
  block.append(text);
  const copy = button("Copy configuration");
  copy.addEventListener("click", async () => {
    try {
      await navigator.clipboard.writeText(companion.hostConfig);
      note("The configuration is on the clipboard.");
    } catch {
      const range = document.createRange();
      range.selectNodeContents(text);
      const selection = window.getSelection();
      selection.removeAllRanges();
      selection.addRange(range);
      note("The configuration is selected; copy it with the keyboard.");
    }
  });
  const actions = element("div", "actions");
  actions.append(copy);
  section.append(element("h2", "", "Host configuration"),
    element("p", "hint", "Add this to an MCP host's mcpServers. The host starts companion-lobby attach, which starts the lobby when it is not running."),
    block, actions);
  return section;
}

function removal(companion) {
  const section = element("section");
  const trigger = button("Remove companion", "danger");
  const count = companion.attachments;
  const question = count
    ? "Remove " + companion.displayName + " and end its " + (count === 1 ? "attachment" : count + " attachments") + "?"
    : "Remove " + companion.displayName + "?";
  confirmInPlace(trigger, question, "Remove", async () => {
    const result = await call("/api/companions/" + encodeURIComponent(companion.id), "DELETE", { expectedRevision: companion.revision, cascade: count > 0 });
    if (!result.ok) {
      note(sentence(result), true);
      if (result.status === 409) refresh(true);
      return;
    }
    lobby.companions.delete(companion.id);
    note(companion.displayName + " was removed.");
    navigate("/");
  });
  section.append(element("h2", "", "Remove"), trigger);
  return section;
}

// ---------- the journal ----------

function who(entry) {
  if (entry.initiator === "sponsor") return "Sponsor";
  if (entry.initiator === "lobby") return "Lobby";
  return "Mind, attachment " + String(entry.attachment || "").slice(0, 8);
}

function whom(entry) {
  if (!entry.companion) return "";
  const companion = lobby.companions.get(entry.companion);
  return companion ? companion.displayName : "companion " + entry.companion.slice(0, 8);
}

function entryLine(entry) {
  const line = element("li");
  const time = element("time", "", new Date(entry.at).toLocaleString());
  time.dateTime = new Date(entry.at).toISOString();
  const what = [who(entry), (entry.command || entry.tool || entry.kind).replace(/_/g, " "), whom(entry)].filter(Boolean).join(" · ");
  const failed = !["done", "accepted", "pending"].includes(entry.outcome);
  line.append(time, element("span", "", what), element("span", "outcome" + (failed ? " failed" : ""), entry.outcome));
  return line;
}

async function loadJournal(before) {
  const query = before ? "?before=" + encodeURIComponent(before) : "";
  return call("/api/journal" + query);
}

function journalArrived(entry) {
  // New entries join the list when it shows the newest ones.
  if (journalView && journalView.live) {
    journalView.entries.unshift(entry);
    if (journalView.list) journalView.list.prepend(entryLine(entry));
  }
}

async function renderJournal() {
  if (journalView) { drawJournal(); return; }
  const mine = journalView = { entries: [], next: null, live: true, list: null };
  view.replaceChildren(element("h1", "", "Journal"), element("p", "muted", "Loading the journal…"));
  const result = await loadJournal(null);
  if (journalView !== mine) return;
  if (!result.ok) { view.replaceChildren(element("h1", "", "Journal"), element("p", "error", sentence(result))); return; }
  mine.entries = result.body.entries;
  mine.next = result.body.next;
  drawJournal();
}

function drawJournal() {
  const state = journalView;
  const list = element("ul", "journal");
  for (const entry of state.entries) list.append(entryLine(entry));
  state.list = list;
  const parts = [element("h1", "", "Journal"),
    element("p", "muted", "Who did what, when, with which outcome; newest first. Entries hold no content and no secrets."),
    list];
  if (state.entries.length === 0) parts.push(element("p", "muted", "Nothing has happened yet."));
  if (state.next) {
    const older = button("Older entries");
    older.addEventListener("click", async () => {
      older.disabled = true;
      const result = await loadJournal(state.next);
      if (journalView !== state) return;
      if (!result.ok) {
        note(sentence(result), true);
        if (result.status === 409) { journalView = null; renderJournal(); }
        older.disabled = false;
        return;
      }
      state.entries.push(...result.body.entries);
      state.next = result.body.next;
      drawJournal();
    });
    const actions = element("div", "actions");
    actions.append(older);
    parts.push(actions);
  }
  view.replaceChildren(...parts);
}

// ---------- start ----------

render();
startFeed();
