"use strict";

// The manager page holds no truth. It loads the lobby, follows the live feed,
// and loads the lobby again whenever the feed says to reset or a change it
// asked for was refused because the lobby changed first. Text reaches the
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

// handoffs holds, in this page's memory only, where the provider's page is
// for each sign-in this page started, and what the sponsor typed.
const lobby = {
  companions: new Map(), credentials: new Map(), signIns: new Map(), kinds: [],
  handoffs: new Map(), journalHealthy: true, position: 0, loaded: false,
};
let resynchronizing = true;
let loads = 0;
// Whether the feed is open; when it was refused or ended for good, the page
// loads the lobby anyway and opens the feed again a few seconds later.
let following = false;
const buffered = [];

// Applies one feed item the loaded lobby does not include yet.
function applyItem(seq, item) {
  if (seq <= lobby.position) return false;
  lobby.position = seq;
  if (item.kind === "companion") lobby.companions.set(item.companion.id, item.companion);
  else if (item.kind === "companion_removed") lobby.companions.delete(item.id);
  else if (item.kind === "credential") lobby.credentials.set(item.credential.id, item.credential);
  else if (item.kind === "sign_in") lobby.signIns.set(item.signIn.id, item.signIn);
  else if (item.kind === "journal_health") lobby.journalHealthy = item.healthy;
  else if (item.kind === "journal") journalArrived(item.entry);
  return true;
}

// Loads the lobby again. Only the latest load applies: a reset that arrives
// while an older load runs makes that load's answer stale, and the items
// buffered meanwhile wait for the latest one.
async function resynchronize() {
  const mine = ++loads;
  resynchronizing = true;
  setLive("Loading", false);
  const result = await call("/api/lobby");
  if (mine !== loads) return;
  if (result.ok) {
    lobby.companions = new Map(result.body.companions.map(companion => [companion.id, companion]));
    lobby.credentials = new Map(result.body.credentials.map(credential => [credential.id, credential]));
    // The lobby lists the sign-ins that wait; the ones this page saw end
    // stay as the page last heard of them.
    const ended = [...lobby.signIns.values()].filter(signIn => signIn.status !== "pending");
    lobby.signIns = new Map(result.body.signIns.map(signIn => [signIn.id, signIn]));
    for (const signIn of ended) if (!lobby.signIns.has(signIn.id)) lobby.signIns.set(signIn.id, signIn);
    lobby.kinds = result.body.kinds;
    lobby.position = result.body.feed.position;
    lobby.journalHealthy = result.body.journal.healthy;
    lobby.loaded = true;
  }
  resynchronizing = false;
  for (const [seq, item] of buffered.splice(0)) applyItem(seq, item);
  setLive(result.ok ? (following ? "Live" : "Loaded; live updates are paused") : "Could not load the lobby; retrying", result.ok && following);
  if (!result.ok) setTimeout(() => { if (mine === loads) resynchronize(); }, 2000);
  refresh(true);
}

function setLive(text, on) {
  const live = document.getElementById("live");
  live.textContent = text;
  live.classList.toggle("on", on);
}

function startFeed() {
  if (!window.EventSource) { setLive("Live updates are not available in this browser", false); return; }
  const source = new EventSource("/api/events");
  source.onopen = () => {
    following = true;
    if (!resynchronizing) setLive("Live", true);
  };
  source.onmessage = message => {
    let item;
    try { item = JSON.parse(message.data); } catch { return; }
    if (item.kind === "reset") { resynchronize(); return; }
    const seq = Number(String(message.lastEventId).split(":").pop());
    if (!Number.isFinite(seq)) return;
    if (resynchronizing) buffered.push([seq, item]);
    else if (applyItem(seq, item)) refresh(false);
  };
  source.onerror = () => {
    following = false;
    if (source.readyState !== EventSource.CLOSED) { setLive("Reconnecting", false); return; }
    source.close();
    resynchronize();
    setTimeout(startFeed, 5000);
  };
}

// A reply holds the companion as the command left it; the feed may already
// have brought a later revision, which the reply must not overwrite.
function keepNewer(companion) {
  const held = lobby.companions.get(companion.id);
  if (!held || held.revision < companion.revision) lobby.companions.set(companion.id, companion);
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
  else if (parts[0] === "companion" && parts.length === 4 && parts[2] === "sign-in") renderSignIn(parts[1], parts[3]);
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
  view.replaceChildren(back, heading, facts, accounts(companion), renameForm(companion), hostConfig(companion), removal(companion));
}

// ---------- accounts: credentials and the sign-ins that make them ----------

function waitingNow(signIn) {
  return signIn.status === "pending" && Date.now() < signIn.expiresAt;
}

function credentialsOf(companion) {
  return [...lobby.credentials.values()].filter(credential => credential.companion === companion.id);
}

function accounts(companion) {
  const section = element("section");
  section.setAttribute("aria-label", "Accounts");
  const parts = [element("h2", "", "Accounts")];
  const held = credentialsOf(companion);
  if (held.length === 0) {
    parts.push(element("p", "muted", "No account is signed in for " + companion.displayName + " yet."));
  } else {
    const list = element("ul", "accounts");
    for (const credential of held) list.append(credentialLine(credential));
    parts.push(list);
  }
  for (const kind of lobby.kinds) {
    const ready = held.some(credential => credential.kind === kind && credential.state === "ready");
    const waiting = [...lobby.signIns.values()].find(signIn => signIn.companion === companion.id && signIn.kind === kind && waitingNow(signIn));
    if (waiting) parts.push(waitingLine(companion, waiting));
    else if (!ready) parts.push(signInForm(companion, kind));
  }
  section.append(...parts);
  return section;
}

function credentialLine(credential) {
  const line = element("li", "account");
  const words = element("div");
  const facts = [credential.kind, credential.state === "ready" ? "signed in" : "revoked"];
  if (credential.permissions.length) facts.push("permissions: " + credential.permissions.join(", "));
  words.append(element("strong", "", credential.account), element("p", "hint", facts.join(" · ")));
  line.append(words);
  if (credential.state === "ready") {
    const trigger = button("Revoke", "danger");
    confirmInPlace(trigger, "Revoke this account for this companion? Every connection made with it is revoked too.", "Revoke", async () => {
      const result = await call("/api/credentials/" + encodeURIComponent(credential.id) + "/revoke", "POST", {});
      if (!result.ok) {
        note(sentence(result), true);
        if (result.status === 409) resynchronize();
        return;
      }
      lobby.credentials.set(credential.id, result.body.credential);
      note("The account was revoked.");
      refresh(true);
    });
    line.append(trigger);
  }
  return line;
}

function waitingLine(companion, signIn) {
  const line = element("p", "warning");
  line.append(document.createTextNode("A sign-in with " + signIn.kind + " is waiting until " + new Date(signIn.expiresAt).toLocaleTimeString() + ". "),
    link("/companion/" + encodeURIComponent(companion.id) + "/sign-in/" + encodeURIComponent(signIn.id), "", "Follow it"));
  return line;
}

function signInForm(companion, kind) {
  const form = element("form");
  form.setAttribute("aria-label", "Sign " + companion.displayName + " in with " + kind);
  const account = field("Account (optional)", "", "What the provider calls the account, such as its handle. Leave it empty to choose the account on the provider's own page.");
  const go = element("button", "primary", "Sign in with " + kind);
  go.type = "submit";
  const actions = element("div", "actions");
  actions.append(go);
  form.append(account.wrapper, actions);
  form.addEventListener("submit", async event => {
    event.preventDefault();
    go.disabled = true;
    const hint = account.input.value.trim();
    const result = await call("/api/sign-ins", "POST", { companion: companion.id, kind, hint });
    go.disabled = false;
    if (!result.ok) {
      note(sentence(result), true);
      if (result.status === 409) resynchronize();
      return;
    }
    const signIn = result.body.signIn;
    lobby.signIns.set(signIn.id, signIn);
    lobby.handoffs.set(signIn.id, { url: result.body.url, hint });
    navigate("/companion/" + encodeURIComponent(companion.id) + "/sign-in/" + encodeURIComponent(signIn.id));
  });
  return form;
}

let expiryTimer = null;

// The page between the lobby and the provider: it names the companion and
// the account the sponsor meant, links to the provider's page, and follows
// the sign-in until it ends.
function renderSignIn(companionId, signInId) {
  clearTimeout(expiryTimer);
  const companion = lobby.companions.get(companionId);
  const signIn = lobby.signIns.get(signInId);
  const handoff = lobby.handoffs.get(signInId);
  if (!companion || !signIn || signIn.companion !== companionId) {
    view.replaceChildren(link("/", "", "All companions"), element("h1", "", "Not here"),
      element("p", "muted", "This page does not know that sign-in; it may have ended. Start one from the companion's page."));
    return;
  }
  const back = link("/companion/" + encodeURIComponent(companion.id), "", companion.displayName);
  const heading = element("h1", "", "Sign " + companion.displayName + " in with " + signIn.kind);
  const meant = handoff && handoff.hint
    ? "The account: " + handoff.hint + ". The provider asks you to sign in as it."
    : "The account: the one you choose on the provider's page.";
  const parts = [back, heading, element("p", "", meant)];
  const signedIn = credentialsOf(companion).find(credential => credential.kind === signIn.kind && credential.state === "ready");
  if (waitingNow(signIn)) {
    parts.push(element("p", "muted", "Waiting for the provider to send you back, until " + new Date(signIn.expiresAt).toLocaleTimeString() + "."));
    const actions = element("div", "actions");
    if (handoff) {
      const onward = element("a", "button primary", "Continue to " + new URL(handoff.url).host);
      onward.href = handoff.url;
      onward.target = "_blank";
      onward.rel = "noopener noreferrer";
      actions.append(onward);
    } else {
      parts.push(element("p", "hint", "The provider's page is linked only from the page that started this sign-in. Cancel it and start again if that page was closed."));
    }
    const cancel = button("Cancel sign-in", "danger");
    confirmInPlace(cancel, "Cancel this sign-in? Nothing is signed in.", "Cancel sign-in", async () => {
      const result = await call("/api/sign-ins/" + encodeURIComponent(signIn.id) + "/cancel", "POST", {});
      if (!result.ok) {
        note(sentence(result), true);
        resynchronize();
        return;
      }
      lobby.signIns.set(signIn.id, result.body.signIn);
      lobby.handoffs.delete(signIn.id);
      note("The sign-in was cancelled.");
      refresh(true);
    }, "Keep waiting");
    actions.append(cancel);
    parts.push(actions);
    expiryTimer = setTimeout(() => refresh(false), Math.max(0, signIn.expiresAt - Date.now()) + 500);
  } else if (signIn.status === "signed_in") {
    parts.push(element("p", "ok", signedIn
      ? "Signed in: " + signedIn.account + " is now " + companion.displayName + "'s " + signIn.kind + " account."
      : "Signed in."));
  } else if (signIn.status === "cancelled") {
    parts.push(element("p", "muted", "This sign-in was cancelled; nothing was signed in."));
  } else if (signIn.status === "refused") {
    parts.push(element("p", "error", "The provider's answer did not complete this sign-in; nothing was signed in. Start it again from the companion's page."));
  } else {
    parts.push(element("p", "muted", "This sign-in expired; nothing was signed in. Start it again from the companion's page."));
  }
  view.replaceChildren(...parts);
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
      if (result.status === 409) resynchronize();
      return;
    }
    keepNewer(result.body.companion);
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
      if (result.status === 409) resynchronize();
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
    if (journalView.entries.some((shown) => shown.seq === entry.seq)) return;
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
  // Entries that arrived while the page loaded and lie above it stay on top;
  // the page already holds the rest.
  const arrived = mine.entries.filter((entry) => entry.seq > result.body.upper);
  mine.entries = arrived.concat(result.body.entries);
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

// The lobby loads once at start whatever the feed does; the feed's own reset
// loads it again, and only the latest load applies.
render();
startFeed();
resynchronize();
