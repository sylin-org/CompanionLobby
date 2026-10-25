"use strict";

// The manager page holds no truth. It loads the lobby, follows the live feed,
// and loads the lobby again whenever the feed says to reset or a change it
// asked for was refused because the lobby changed first. Text reaches the
// page only through textContent; nothing becomes markup. What the sponsor
// types is kept as a draft in this tab's own storage, so a reload loses
// nothing; everything else the page shows comes from the lobby. While the
// sponsor types in a form or answers a question asked in place, what the
// lobby changes waits, says so, and shows once the sponsor leaves; after
// every action the focus lands where the result is.

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

let fieldCount = 0;

// A labelled input; the hint is tied to it for assistive technology.
function field(label, value, hint, options = {}) {
  const id = "field-" + (++fieldCount);
  const wrapper = element("div", "field");
  const caption = element("label", "", label);
  caption.htmlFor = id;
  const input = element("input");
  input.id = id;
  input.type = options.type || "text";
  input.value = value || "";
  input.autocomplete = "off";
  input.spellcheck = false;
  if (options.placeholder) input.placeholder = options.placeholder;
  if (options.required) input.required = true;
  wrapper.append(caption, input);
  if (hint) {
    const said = element("span", "hint", hint);
    said.id = id + "-hint";
    input.setAttribute("aria-describedby", said.id);
    wrapper.append(said);
  }
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

// A sentence that ends a name with its own stop is not stopped twice.
function tidy(text) {
  return String(text).replace(/([.!?])\.(?=\s|$)/g, "$1");
}

function note(message, isError) {
  const line = document.getElementById("notice");
  line.textContent = tidy(message);
  line.className = isError ? "error" : "ok";
  clearTimeout(note.timer);
  note.timer = setTimeout(() => { line.textContent = ""; }, 6000);
}

// A refusal of what a form asked says so inside that form, beside its
// button, until the sponsor types again; the focus goes back to its first
// field.
function formSays(form, message) {
  let line = form.querySelector(".form-said");
  if (!line) {
    line = element("p", "form-said error");
    line.setAttribute("role", "alert");
    form.append(line);
  }
  line.textContent = tidy(message);
  form.addEventListener("input", () => line.remove(), { once: true });
  const first = form.querySelector("input, select");
  if (first) first.focus();
}

// Runs a form's submission, marking the form busy meanwhile so what the
// lobby changes keeps waiting even though the disabled button lost focus.
async function submitting(form, control, work) {
  form.dataset.busy = "1";
  control.disabled = true;
  try { await work(); } finally {
    control.disabled = false;
    delete form.dataset.busy;
    setTimeout(settle, 0);
  }
}

// A destructive action asks in place, where the thing lives; the page never
// opens a dialog. The trigger keeps the focus whichever way it is answered.
function confirmInPlace(trigger, question, confirmLabel, action, cancelLabel = "Keep") {
  const group = element("span", "confirm-inline");
  group.setAttribute("role", "group");
  group.setAttribute("aria-label", question);
  const yes = button(confirmLabel, "danger");
  const keep = button(cancelLabel, "quiet");
  group.append(element("span", "confirm-question", question), yes, keep);
  const restore = () => { if (group.isConnected) group.replaceWith(trigger); trigger.focus(); };
  trigger.addEventListener("click", () => { trigger.replaceWith(group); yes.focus(); });
  yes.addEventListener("click", () => { restore(); Promise.resolve(action()).finally(() => setTimeout(settle, 0)); });
  keep.addEventListener("click", () => { restore(); settle(); });
  group.addEventListener("keydown", event => {
    if (event.key === "Escape") { event.stopPropagation(); restore(); settle(); }
  });
}

// Drafts live in this tab's own storage, which may be refused.
const drafts = {
  read(key) {
    try { return JSON.parse(sessionStorage.getItem("draft:" + key) || "null"); } catch { return null; }
  },
  write(key, value) {
    try { sessionStorage.setItem("draft:" + key, JSON.stringify(value)); } catch { /* a draft is a convenience */ }
  },
  clear(key) {
    try { sessionStorage.removeItem("draft:" + key); } catch { /* a draft is a convenience */ }
  },
};

// A moniker suggested from a display name: lowercase ASCII, single hyphens.
function monikerFrom(name) {
  return name.trim().toLowerCase().normalize("NFKD").replace(/[̀-ͯ]/g, "")
    .replace(/[^a-z0-9]+/g, "-").replace(/^[^a-z]+/, "").replace(/-+$/g, "").slice(0, 24).replace(/-+$/g, "");
}

function hue(id) {
  return Array.from(id || "").reduce((sum, letter) => sum + letter.charCodeAt(0), 0) % 360;
}

function initials(name) {
  const words = String(name || "?").trim().split(/\s+/).filter(Boolean);
  const letters = words.length > 1 ? [words[0], words[words.length - 1]] : [words[0] || "?"];
  return letters.map(word => Array.from(word)[0].toUpperCase()).join("");
}

// A portrait, or the companion's initials when it has none. Only a path of
// this lobby's own becomes an image.
function avatar(companion, size) {
  const node = element("span", "avatar" + (size ? " " + size : ""), initials(companion.displayName || companion.moniker));
  node.setAttribute("aria-hidden", "true");
  node.style.setProperty("--hue", String(hue(companion.id || companion.displayName)));
  const source = companion.portrait;
  if (typeof source === "string" && source.startsWith("/") && !source.startsWith("//")) {
    const image = element("img");
    image.alt = "";
    image.src = source;
    image.addEventListener("error", () => image.remove());
    node.append(image);
  }
  return node;
}

function attachedText(count) {
  if (!count) return "No mind attached";
  return count === 1 ? "1 mind attached" : count + " minds attached";
}

function timeText(millis) {
  const when = new Date(millis);
  const sameDay = when.toDateString() === new Date().toDateString();
  return sameDay
    ? when.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })
    : when.toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
}

// The address the sponsor typed, without its scheme.
function placeText(address) {
  return String(address || "").replace(/^[a-z]+:\/\//, "");
}

function companionPath(companion, rest) {
  return "/companion/" + encodeURIComponent(companion.id) + (rest || "");
}

// ---------- the lobby: loaded, then patched by the feed ----------

const lobby = {
  companions: new Map(), credentials: new Map(), connections: new Map(), signIns: new Map(),
  kinds: [], services: [], journalHealthy: true, position: 0, loaded: false,
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
  else if (item.kind === "connection") lobby.connections.set(item.connection.id, item.connection);
  else if (item.kind === "sign_in") lobby.signIns.set(item.signIn.id, item.signIn);
  else if (item.kind === "journal_health") lobby.journalHealthy = item.healthy;
  else if (item.kind === "journal") { journalArrived(item.entry); return false; }
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
    const body = result.body;
    lobby.companions = new Map(body.companions.map(companion => [companion.id, companion]));
    lobby.credentials = new Map(body.credentials.map(credential => [credential.id, credential]));
    lobby.connections = new Map(body.connections.map(connection => [connection.id, connection]));
    lobby.signIns = new Map(body.signIns.map(signIn => [signIn.id, signIn]));
    lobby.kinds = body.kinds;
    lobby.services = body.services;
    lobby.position = body.feed.position;
    lobby.journalHealthy = body.journal.healthy;
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

// A reply holds the record as the command left it; the feed may already
// have brought a later revision, which the reply must not overwrite.
function keepNewer(companion) {
  const held = lobby.companions.get(companion.id);
  if (!held || held.revision < companion.revision) lobby.companions.set(companion.id, companion);
}

// ---------- what the lobby says about one companion ----------

function kindTitle(kind) {
  const found = lobby.kinds.find(entry => entry.kind === kind);
  return found ? found.title : kind;
}

function serviceTitle(name) {
  const found = lobby.services.find(entry => entry.name === name);
  return found ? found.title : name;
}

function credentialsOf(companion) {
  return [...lobby.credentials.values()].filter(credential => credential.companion === companion.id);
}

function readyCredential(companion, kind) {
  return credentialsOf(companion).find(credential => credential.kind === kind && credential.state === "ready");
}

// The credential of a kind that waits for the sponsor to sign it in again.
function waitingCredential(companion, kind) {
  return credentialsOf(companion).find(credential => credential.kind === kind && credential.state === "needs_sign_in");
}

// The accounts a companion holds now: ready, or waiting to be signed in again.
function liveCredentials(companion) {
  return credentialsOf(companion).filter(credential => credential.state !== "revoked");
}

function waitingNow(signIn) {
  return signIn.status === "pending" && Date.now() < signIn.expiresAt;
}

function waitingSignIn(companion, kind) {
  return [...lobby.signIns.values()].find(signIn => signIn.companion === companion.id && signIn.kind === kind && waitingNow(signIn));
}

function connectionsOf(companion) {
  return [...lobby.connections.values()].filter(connection => connection.companion === companion.id);
}

// The one honest word for a connection, and the one action beside it.
function connectionWord(connection) {
  if (connection.state === "revoked") return "revoked";
  const credential = lobby.credentials.get(connection.credential);
  return connection.state === "ready" && credential && credential.state === "ready" ? "ready" : "sign_in_needed";
}

const WORDS = { ready: "Ready", sign_in_needed: "Sign in needed", revoked: "Revoked" };

// The connections worth showing: every one that stands, ready or waiting
// for a sign-in, and a revoked one only while nothing standing or newer
// holds its name.
function shownConnections(companion) {
  const all = connectionsOf(companion);
  return all.filter((connection, index) => {
    if (connection.state !== "revoked") return true;
    return !all.some((other, at) => other !== connection && other.service === connection.service && other.label === connection.label && (other.state !== "revoked" || at > index));
  });
}

// What the companion's page offers first: the single primary action.
function nextStep(companion) {
  if (lobby.kinds.some(entry => waitingCredential(companion, entry.kind) && !waitingSignIn(companion, entry.kind))) return "sign-in-again";
  const anyAccount = lobby.kinds.some(entry => readyCredential(companion, entry.kind) || waitingSignIn(companion, entry.kind));
  if (!anyAccount) return "sign-in";
  const usable = lobby.services.some(service => service.accepts.some(kind => readyCredential(companion, kind)));
  const reaching = connectionsOf(companion).some(connection => connection.state === "ready");
  if (usable && !reaching) return "connect";
  return null;
}

// ---------- the router: every state of the page is a route ----------

const view = document.getElementById("view");
let editing = false;
let refreshPending = false;
let journalView = null;
let expiryTimer = null;
// Which disclosures the sponsor opened, so a refresh keeps them open.
const opened = new Set();

view.addEventListener("input", () => { editing = true; });
view.addEventListener("focusout", event => {
  const form = event.target.closest && event.target.closest("form");
  if (!form || (event.relatedTarget && form.contains(event.relatedTarget))) return;
  setTimeout(settle, 0);
});

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
  window.scrollTo(0, 0);
}

// Whether the sponsor is in the middle of something a refresh would undo:
// typing in a form, a form's submission, or a question asked in place.
function busyHere() {
  if (view.querySelector("form[data-busy], .confirm-inline")) return true;
  const at = document.activeElement;
  return editing && !!(at && view.contains(at) && at.closest("form"));
}

// A feed update waits while the sponsor is busy, unless it must not, and
// says so where the sponsor is. `focusKey` names where the focus lands
// after it; without one, the focus stays on the control it was on.
function refresh(force, focusKey) {
  if (!force && busyHere()) { refreshPending = true; sayWaiting(); return; }
  editing = false;
  refreshPending = false;
  const at = document.activeElement;
  const focused = focusKey || (at && at.dataset ? at.dataset.key : null);
  render();
  if (focused) {
    const again = view.querySelector('[data-key="' + focused + '"]');
    if (again) again.focus({ preventScroll: true });
  }
}

// The sponsor left what kept the lobby's changes waiting: they show now.
function settle() {
  if (busyHere()) return;
  editing = false;
  if (refreshPending) refresh(true);
}

// Says, where the sponsor is, that the lobby changed meanwhile.
function sayWaiting() {
  const at = document.activeElement;
  const here = at && view.contains(at) ? at.closest("form") || at.closest(".confirm-inline") : null;
  const place = here || view.querySelector("form[data-busy], .confirm-inline");
  if (!place || place.querySelector(".waiting")) return;
  const inForm = place.tagName === "FORM";
  const line = element(inForm ? "p" : "span", "hint waiting", inForm
    ? "The lobby changed; it shows when you leave this form."
    : "The lobby changed; it shows once you answer.");
  line.setAttribute("role", "status");
  place.append(line);
}

function render() {
  clearTimeout(expiryTimer);
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
  if (parts.length === 0) renderLobby();
  else if (parts[0] === "journal" && parts.length === 1) renderJournalPage();
  else if (parts[0] === "companion" && parts[1] && parts.length === 2) renderCompanion(parts[1]);
  else if (parts[0] === "companion" && parts.length === 3 && parts[2] === "attach") renderAttach(parts[1]);
  else if (parts[0] === "companion" && parts.length === 4 && parts[2] === "sign-in") renderSignIn(parts[1], parts[3]);
  else { history.replaceState({}, "", "/"); renderLobby(); }
}

function notHere(text) {
  view.replaceChildren(link("/", "back", "All companions"), element("h1", "", "Not here"), element("p", "muted", text));
}

// ---------- the lobby ----------

function renderLobby() {
  const companions = [...lobby.companions.values()];
  if (companions.length === 0) { renderFirstRun(); return; }
  const list = element("ul", "cards");
  for (const companion of companions) list.append(card(companion));
  const parts = [element("h1", "", "Companions"), list];
  const draft = drafts.read("create");
  const adding = element("section", "adding");
  adding.setAttribute("aria-label", "Add a companion");
  const open = button("Add a companion", "primary");
  open.dataset.key = "add";
  const form = createForm(false);
  form.hidden = !draft;
  open.hidden = !!draft;
  open.addEventListener("click", () => { open.hidden = true; form.hidden = false; form.querySelector("input").focus(); });
  form.addEventListener("reset", () => { drafts.clear("create"); form.hidden = true; open.hidden = false; open.focus(); });
  adding.append(open, form);
  parts.push(adding);
  view.replaceChildren(...parts);
}

function card(companion) {
  const item = element("li", "card");
  const words = element("div", "card-words");
  const name = link(companionPath(companion), "card-name", companion.displayName);
  name.dataset.key = "card-" + companion.id;
  const facts = [attachedText(companion.attachments)];
  const accounts = liveCredentials(companion);
  if (accounts.length === 0) facts.push("No account yet");
  const ready = connectionsOf(companion).filter(connection => connectionWord(connection) === "ready").length;
  if (ready) facts.push(ready === 1 ? "1 connection ready" : ready + " connections ready");
  const waiting = connectionsOf(companion).filter(connection => connectionWord(connection) === "sign_in_needed").length;
  if (waiting) facts.push(waiting === 1 ? "1 connection needs a sign-in" : waiting + " connections need a sign-in");
  words.append(name, element("p", "", facts.join(" · ")));
  if (accounts.length) {
    const list = element("ul", "card-accounts");
    list.setAttribute("aria-label", "Accounts of " + companion.displayName);
    for (const credential of accounts) list.append(cardAccount(companion, credential));
    words.append(list);
  }
  item.append(avatar(companion), words);
  return item;
}

// One account on a companion's card: Revoke while it is signed in; while it
// waits for a sign-in, the way back (Sign in again, or Continue when that
// sign-in already waits) and Revoke beside it.
function cardAccount(companion, credential) {
  const line = element("li", "card-account");
  const ready = credential.state === "ready";
  const said = element("span", ready ? "" : "warn", credential.label + " · " + kindTitle(credential.kind) + (ready ? "" : " · Sign in needed"));
  const revoke = button("Revoke", ready ? "danger" : "quiet");
  revoke.dataset.key = "card-revoke-" + credential.id;
  confirmInPlace(revoke, revokeQuestion(companion, credential), "Revoke", () => revokeCredential(companion, credential, "card-" + companion.id));
  if (ready) {
    line.append(said, revoke);
    return line;
  }
  const pending = waitingSignIn(companion, credential.kind);
  let back;
  if (pending) {
    back = link(companionPath(companion, "/sign-in/" + encodeURIComponent(pending.id)), "button", "Continue sign-in");
    back.dataset.key = "card-continue-" + pending.id;
  } else {
    back = button("Sign in again");
    back.dataset.key = "card-again-" + credential.id;
    back.addEventListener("click", () => signInAgain(companion, credential, back));
  }
  const actions = element("span", "actions");
  actions.append(back, revoke);
  line.append(said, actions);
  return line;
}

// Starts signing the same account in again and leads to the handoff.
async function signInAgain(companion, credential, control) {
  control.disabled = true;
  const handoff = await startSignIn(companion, credential.kind, "");
  control.disabled = false;
  if (handoff) navigate(handoff);
  else control.focus();
}

function revokeQuestion(companion, credential) {
  const derived = connectionsOf(companion).filter(connection => connection.credential === credential.id && connection.state !== "revoked").length;
  return "Revoke " + credential.label + " for " + companion.displayName + "?" + (derived ? " Its connections are revoked too." : "");
}

function renderFirstRun() {
  const intro = element("p", "lede", "A companion is who a mind attaches as. Give yours a name; you can change it later.");
  view.replaceChildren(element("h1", "", "Name your first companion"), intro, createForm(true));
}

// The form that names a new companion. Its draft survives a reload.
function createForm(first) {
  const form = element("form", "panel");
  form.setAttribute("aria-label", "Name a companion");
  form.noValidate = true;
  const draft = drafts.read("create") || {};
  const preview = avatar({ displayName: draft.name || "?", id: draft.name || "" }, "large");
  const name = field("Display name", draft.name, "What people read, such as Ada.", { required: true });
  const moniker = field("Moniker", draft.moniker, "How a host names it: lowercase letters, digits and single hyphens.");
  const portrait = field("Portrait (optional)", "", "A PNG, JPEG, WebP or GIF image of at most 512 KiB. Without one, the initials show.", { type: "file" });
  portrait.input.accept = "image/png,image/jpeg,image/webp,image/gif";
  name.input.dataset.key = "create-name";
  moniker.input.dataset.key = "create-moniker";
  let monikerEdited = !!draft.monikerEdited;
  let chosen = null;
  let shownUrl = null;
  const save = () => drafts.write("create", { name: name.input.value, moniker: moniker.input.value, monikerEdited });
  const showPreview = () => {
    const next = avatar({ displayName: name.input.value.trim() || "?", id: name.input.value }, "large");
    if (shownUrl) URL.revokeObjectURL(shownUrl);
    shownUrl = null;
    if (chosen) {
      const image = element("img");
      image.alt = "";
      shownUrl = URL.createObjectURL(chosen);
      image.src = shownUrl;
      next.append(image);
    }
    previewHolder.replaceChildren(next);
  };
  const previewHolder = element("div", "preview");
  previewHolder.append(preview);
  name.input.addEventListener("input", () => {
    if (!monikerEdited) moniker.input.value = monikerFrom(name.input.value);
    save();
    showPreview();
  });
  moniker.input.addEventListener("input", () => { monikerEdited = true; save(); });
  portrait.input.addEventListener("change", () => {
    chosen = portrait.input.files && portrait.input.files[0] ? portrait.input.files[0] : null;
    showPreview();
  });
  const create = element("button", "primary", "Create companion");
  create.type = "submit";
  const actions = element("div", "actions");
  actions.append(create);
  if (!first) {
    const cancel = element("button", "quiet", "Cancel");
    cancel.type = "reset";
    actions.append(cancel);
  }
  const fields = element("div", "fields");
  fields.append(name.wrapper, moniker.wrapper, portrait.wrapper);
  const row = element("div", "create-row");
  row.append(previewHolder, fields);
  form.append(row, actions);
  form.addEventListener("submit", async event => {
    event.preventDefault();
    const displayName = name.input.value.trim();
    const wanted = moniker.input.value.trim() || monikerFrom(displayName);
    if (!displayName) { formSays(form, "Give the companion a display name first."); return; }
    await submitting(form, create, async () => {
      const result = await call("/api/companions", "POST", { moniker: wanted, displayName });
      if (!result.ok) { formSays(form, sentence(result)); return; }
      let companion = result.body.companion;
      keepNewer(companion);
      drafts.clear("create");
      editing = false;
      if (chosen) {
        // A portrait that is not kept leaves a note on the pages that come
        // next, with the way to choose it again.
        const uploaded = await uploadPortrait(companion, chosen, true);
        if (uploaded) companion = uploaded;
      }
      navigate(companionPath(companion, "/attach"));
    });
  });
  return form;
}

function readAsDataUrl(file) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(reader.result);
    reader.onerror = () => reject(reader.error);
    reader.readAsDataURL(file);
  });
}

// Sends a portrait; a refusal is said at once, or, when `lasting`, kept as
// a note the companion's pages show until a portrait is kept or the sponsor
// keeps the initials.
async function uploadPortrait(companion, file, lasting) {
  const failed = message => {
    if (lasting) drafts.write("portrait-failed:" + companion.id, message);
    else note(message, true);
    return null;
  };
  if (file.size > 512 * 1024) return failed("That image is too large; a portrait is at most 512 KiB.");
  let image;
  try { image = await readAsDataUrl(file); } catch { return failed("That file could not be read."); }
  const result = await call(companionPath(companion).replace("/companion/", "/api/companions/") + "/portrait", "PUT", { expectedRevision: companion.revision, image });
  if (!result.ok) { if (result.status === 409) resynchronize(); return failed(sentence(result)); }
  drafts.clear("portrait-failed:" + companion.id);
  keepNewer(result.body.companion);
  return result.body.companion;
}

// The note a portrait that was not kept leaves, with the way to choose it
// again and the way to keep the initials, or nothing.
function portraitNote(companion) {
  const said = drafts.read("portrait-failed:" + companion.id);
  if (!said) return null;
  const box = element("div", "panel portrait-note");
  const line = element("p", "status warn", tidy("The portrait was not kept: " + said + " " + companion.displayName + " shows its initials until you choose one again."));
  line.setAttribute("role", "status");
  const choose = button("Choose a portrait");
  choose.dataset.key = "portrait-again-" + companion.id;
  choose.addEventListener("click", () => {
    opened.add("settings-" + companion.id);
    if (location.pathname !== companionPath(companion)) history.pushState({}, "", companionPath(companion));
    editing = false;
    refresh(true, "portrait-" + companion.id);
  });
  const keep = button("Keep the initials", "quiet");
  keep.dataset.key = "portrait-keep-" + companion.id;
  keep.addEventListener("click", () => {
    drafts.clear("portrait-failed:" + companion.id);
    refresh(true);
    view.focus({ preventScroll: true });
  });
  const actions = element("div", "actions");
  actions.append(choose, keep);
  box.append(line, actions);
  return box;
}

// ---------- first attachment: what just happened and what comes next ----------

function renderAttach(id) {
  const companion = lobby.companions.get(id);
  if (!companion) { notHere("No companion has that id; it may have been removed."); return; }
  const parts = [link(companionPath(companion), "back", companion.displayName)];
  const heading = element("div", "identity");
  heading.append(avatar(companion, "large"), element("h1", "", companion.displayName + (companion.attachments > 0 ? " is attached" : " is ready to attach")));
  parts.push(heading);
  const trouble = portraitNote(companion);
  if (trouble) parts.push(trouble);
  parts.push(element("p", "lede", "Copy this block into your AI host's server settings. The host starts the lobby itself whenever it needs it."));
  const block = element("div", "config");
  const text = element("pre", "", companion.hostConfig);
  text.tabIndex = 0;
  text.setAttribute("aria-label", "The configuration block for " + companion.displayName);
  block.append(text);
  parts.push(block);
  const attached = companion.attachments > 0;
  const copy = button("Copy block", attached ? "" : "primary");
  copy.dataset.key = "copy";
  copy.addEventListener("click", () => copyBlock(companion, text));
  const actions = element("div", "actions");
  actions.append(copy);
  parts.push(actions);
  const status = element("p", "status" + (attached ? " ok" : ""));
  status.setAttribute("role", "status");
  if (attached) {
    status.textContent = tidy("A mind is attached as " + companion.displayName + ". Next, lend " + companion.displayName + " an account.");
    const next = link(companionPath(companion), "button primary", "Lend " + companion.displayName + " an account");
    next.dataset.key = "lend";
    parts.push(status, next);
  } else {
    status.textContent = tidy("No mind is attached as " + companion.displayName + " yet. This page changes when one attaches.");
    parts.push(status);
  }
  view.replaceChildren(...parts);
}

async function copyBlock(companion, text) {
  try {
    await navigator.clipboard.writeText(companion.hostConfig);
    note("The block is on the clipboard.");
  } catch {
    const range = document.createRange();
    range.selectNodeContents(text);
    const selection = window.getSelection();
    selection.removeAllRanges();
    selection.addRange(range);
    text.focus();
    note("The block is selected; copy it with the keyboard.");
  }
}

// ---------- one companion: accounts, connections, journal ----------

function renderCompanion(id) {
  const companion = lobby.companions.get(id);
  if (!companion) { notHere("No companion has that id; it may have been removed."); return; }
  const step = nextStep(companion);
  const head = element("header", "identity");
  const words = element("div");
  words.append(element("h1", "", companion.displayName), element("p", "muted", companion.moniker + " · " + attachedText(companion.attachments)));
  head.append(avatar(companion, "large"), words);
  const tools = element("div", "head-tools");
  tools.append(link(companionPath(companion, "/attach"), "", "Host configuration"), settings(companion));
  const trouble = portraitNote(companion);
  view.replaceChildren(...[link("/", "back", "All companions"), head, trouble, tools,
    accountsSection(companion, step), connectionsSection(companion, step), journalSection(companion)].filter(Boolean));
  loadCompanionJournal(companion);
}

// ---------- accounts ----------

function accountsSection(companion, step) {
  const section = element("section", "block");
  section.setAttribute("aria-labelledby", "accounts-title");
  const title = element("h2", "", "Accounts");
  title.id = "accounts-title";
  section.append(title);
  if (lobby.kinds.length === 0) {
    section.append(element("p", "muted", "No kind of account is installed in this lobby."));
    return section;
  }
  const list = element("ul", "rows");
  for (const entry of lobby.kinds) list.append(accountRow(companion, entry, step === "sign-in" && entry === lobby.kinds[0], step === "sign-in-again"));
  section.append(list);
  return section;
}

// The words of an account row: its label, its kind, and its key kept for the
// details.
function accountWords(credential, entry) {
  const words = element("div", "row-words");
  words.append(element("strong", "", credential.label), element("span", "kind", entry.title + " account"));
  const details = disclosure("details", "Details", "details-" + credential.id);
  const facts = element("dl");
  facts.append(element("dt", "", "Account key"), element("dd", "mono", credential.account));
  details.append(facts);
  words.append(details);
  return { words, details };
}

function accountRow(companion, entry, primary, again) {
  const row = element("li", "row");
  row.id = "account-" + entry.kind;
  const credential = readyCredential(companion, entry.kind);
  const waiting = waitingSignIn(companion, entry.kind);
  const stale = waitingCredential(companion, entry.kind);
  if (credential) {
    const { words } = accountWords(credential, entry);
    const revoke = button("Revoke", "danger");
    revoke.dataset.key = "revoke-" + credential.id;
    revoke.dataset.signIn = "";
    confirmInPlace(revoke, revokeQuestion(companion, credential), "Revoke", () => revokeCredential(companion, credential));
    row.append(words, element("span", "word ok", "Signed in"), revoke);
    return row;
  }
  if (stale && !waiting) {
    // The way back is signing the same account in again; revoking it
    // instead sits beside it, quieter.
    const { words } = accountWords(stale, entry);
    const instead = button("Revoke", "quiet");
    instead.dataset.key = "revoke-" + stale.id;
    confirmInPlace(instead, revokeQuestion(companion, stale), "Revoke", () => revokeCredential(companion, stale));
    const signIn = button("Sign in again", again ? "primary" : "");
    signIn.dataset.key = "again-" + stale.id;
    signIn.dataset.signIn = "";
    signIn.addEventListener("click", () => signInAgain(companion, stale, signIn));
    const actions = element("span", "actions");
    actions.append(signIn, instead);
    row.append(words, element("span", "word warn", "Sign in needed"), actions);
    return row;
  }
  if (waiting) {
    const words = element("div", "row-words");
    words.append(element("strong", "", waiting.account || "An account of your choice"), element("span", "kind", entry.title + " account"));
    const follow = link(companionPath(companion, "/sign-in/" + encodeURIComponent(waiting.id)), "button" + (primary ? " primary" : ""), "Continue");
    follow.dataset.key = "continue-" + waiting.id;
    follow.dataset.signIn = "";
    row.append(words, element("span", "word", "Signing in"), follow);
    return row;
  }
  const revoked = credentialsOf(companion).filter(credential => credential.kind === entry.kind).pop();
  row.classList.add("row-form");
  if (revoked) row.append(element("p", "muted wide", revoked.label + " was revoked. Sign " + companion.displayName + " in again to lend an account."));
  row.append(signInForm(companion, entry, primary));
  return row;
}

// A disclosure that stays open across refreshes once the sponsor opens it.
function disclosure(className, summary, key) {
  const details = element("details", className);
  const handle = element("summary", "", summary);
  handle.dataset.key = key;
  details.append(handle);
  details.open = opened.has(key);
  details.addEventListener("toggle", () => { if (details.open) opened.add(key); else opened.delete(key); });
  return details;
}

// What the provider said, in the sponsor's words, after the lobby revoked
// an account on its side.
const AT_PROVIDER = {
  confirmed: title => " " + title + " confirmed it is revoked there too.",
  not_offered: title => " " + title + " offers no way to revoke it there, so it ends there when it expires.",
  not_confirmed: title => " " + title + " did not confirm it, so it ends there when it expires.",
  sent_last_known: title => " It waited for a sign-in, so " + title + " was sent the last key the lobby held, which may already have been replaced: the lobby cannot tell whether it is revoked there. The apps your " + title + " account allows are listed in its own settings.",
  not_asked: title => " The lobby held no key for it, so " + title + " was not asked to revoke it; it ends there when it expires.",
};

async function revokeCredential(companion, credential, focusKey) {
  const result = await call("/api/credentials/" + encodeURIComponent(credential.id) + "/revoke", "POST", {});
  if (!result.ok) {
    note(sentence(result), true);
    if (result.status === 409) resynchronize();
    return;
  }
  lobby.credentials.set(credential.id, result.body.credential);
  for (const connection of lobby.connections.values()) {
    if (connection.credential === credential.id) lobby.connections.set(connection.id, Object.assign({}, connection, { state: "revoked" }));
  }
  const said = AT_PROVIDER[result.body.provider] || AT_PROVIDER.not_asked;
  note(credential.label + " is revoked for " + companion.displayName + "." + said(kindTitle(credential.kind)));
  refresh(true, focusKey || "sign-in:" + companion.id + ":" + credential.kind);
}

function signInForm(companion, entry, primary) {
  const form = element("form", "inline-form");
  form.setAttribute("aria-label", "Sign " + companion.displayName + " in with " + entry.title);
  const key = "sign-in:" + companion.id + ":" + entry.kind;
  const account = field(entry.title + " account", drafts.read(key) || "", entry.hint || "The account to sign in with.");
  account.input.dataset.key = key;
  account.input.addEventListener("input", () => drafts.write(key, account.input.value));
  const go = element("button", primary ? "primary" : "", "Sign in with " + entry.title);
  go.type = "submit";
  go.dataset.key = key + ":go";
  form.append(account.wrapper, go);
  form.addEventListener("submit", async event => {
    event.preventDefault();
    await submitting(form, go, async () => {
      const handoff = await startSignIn(companion, entry.kind, account.input.value.trim(), form);
      if (!handoff) return;
      drafts.clear(key);
      editing = false;
      navigate(handoff);
    });
  });
  return form;
}

// Starts a sign-in and answers the path of its handoff, or says why not,
// inside `form` when one asked, and answers null.
async function startSignIn(companion, kind, hint, form) {
  const result = await call("/api/sign-ins", "POST", { companion: companion.id, kind, hint });
  if (!result.ok) {
    if (result.status === 409) { note(sentence(result), true); resynchronize(); }
    else if (form) formSays(form, sentence(result));
    else note(sentence(result), true);
    return null;
  }
  const signIn = result.body.signIn;
  lobby.signIns.set(signIn.id, signIn);
  handoffs.write(signIn.id, result.body.url);
  return companionPath(companion, "/sign-in/" + encodeURIComponent(signIn.id));
}

// Where the sign-in page is, kept in this tab only: the lobby never shows it.
const handoffs = {
  read(id) { try { return sessionStorage.getItem("handoff:" + id); } catch { return null; } },
  write(id, url) { try { sessionStorage.setItem("handoff:" + id, url); } catch { /* the tab that started it shows the link */ } },
  clear(id) { try { sessionStorage.removeItem("handoff:" + id); } catch { /* nothing kept */ } },
};

// The page between the lobby and the provider. It names the companion, the
// account the sponsor meant, as the lobby remembers it, and what is being
// granted, and follows the sign-in until it ends.
function renderSignIn(companionId, signInId) {
  const companion = lobby.companions.get(companionId);
  const signIn = lobby.signIns.get(signInId);
  if (!companion) { notHere("No companion has that id; it may have been removed."); return; }
  const back = link(companionPath(companion), "back", companion.displayName);
  if (!signIn || signIn.companion !== companionId) {
    view.replaceChildren(back, element("h1", "", "This sign-in is not remembered"),
      element("p", "", "The lobby does not remember this sign-in, so it cannot say how it ended. The accounts of " + companion.displayName + " show what is signed in."),
      link(companionPath(companion), "button primary", "Back to " + companion.displayName));
    return;
  }
  const title = kindTitle(signIn.kind);
  const parts = [back, element("h1", "", "Sign " + companion.displayName + " in with " + title)];
  const meant = element("p", "lede");
  meant.append(document.createTextNode("The account: "), element("strong", "", placeText(signIn.account) || "the one you choose on the sign-in page"), document.createTextNode("."));
  parts.push(meant, element("p", "", companion.displayName + " receives this account's identity and nothing more. No connection opens until you grant one."));
  const status = element("p", "status");
  status.setAttribute("role", "status");
  status.tabIndex = -1;
  status.dataset.key = "status";
  parts.push(status);
  const actions = element("div", "actions");
  let after = null;
  if (waitingNow(signIn)) {
    status.textContent = "Waiting for the sign-in page to send you back, until " + timeText(signIn.expiresAt) + ".";
    const url = handoffs.read(signIn.id);
    if (url) {
      const onward = element("a", "button primary", "Continue to sign in");
      onward.href = url;
      onward.target = "_blank";
      onward.rel = "noopener noreferrer";
      onward.dataset.key = "onward";
      actions.append(onward);
      let host = "";
      try { host = new URL(url).hostname; } catch { host = ""; }
      if (host) after = element("p", "hint", "Opens " + host + " in a new tab. Come back here when it is done.");
    } else {
      parts.push(element("p", "hint", "The sign-in page opens from the tab that started this sign-in. Finish there, or start it again here."));
      const again = button("Start this sign-in again here", "primary");
      again.dataset.key = "again";
      again.addEventListener("click", async () => {
        again.disabled = true;
        const ended = await endSignIn(signIn);
        const handoff = ended && await startSignIn(companion, signIn.kind, signIn.account || "");
        again.disabled = false;
        if (handoff) navigate(handoff);
        else refresh(true, "status");
      });
      actions.append(again);
    }
    const cancel = button("Cancel sign-in", url ? "danger" : "quiet");
    cancel.dataset.key = "cancel";
    confirmInPlace(cancel, "Cancel this sign-in? Nothing is signed in.", "Cancel sign-in", async () => {
      if (!await endSignIn(signIn)) return;
      note("The sign-in was cancelled.");
      refresh(true, "status");
    }, "Keep waiting");
    actions.append(cancel);
    expiryTimer = setTimeout(() => refresh(false), Math.max(0, signIn.expiresAt - Date.now()) + 500);
  } else {
    const credential = readyCredential(companion, signIn.kind);
    const said = {
      signed_in: credential
        ? "Signed in. The " + title + " account for " + companion.displayName + " is " + credential.label + "."
        : "Signed in.",
      cancelled: "This sign-in was cancelled. Nothing was signed in.",
      refused: "The sign-in page's answer did not complete this sign-in. Nothing was signed in.",
    }[signIn.status] || "This sign-in expired before it came back. Nothing was signed in.";
    status.textContent = tidy(said);
    status.className = "status" + (signIn.status === "signed_in" ? " ok" : "");
    handoffs.clear(signIn.id);
    const onward = link(companionPath(companion), "button primary", signIn.status === "signed_in" ? "Back to " + companion.displayName : "Start again from " + companion.displayName);
    onward.dataset.key = "back";
    actions.append(onward);
  }
  parts.push(actions);
  if (after) parts.push(after);
  view.replaceChildren(...parts);
}

// Cancels a waiting sign-in; answers whether it ended.
async function endSignIn(signIn) {
  const result = await call("/api/sign-ins/" + encodeURIComponent(signIn.id) + "/cancel", "POST", {});
  if (!result.ok) { note(sentence(result), true); resynchronize(); return false; }
  lobby.signIns.set(signIn.id, result.body.signIn);
  handoffs.clear(signIn.id);
  return true;
}

// ---------- connections ----------

function connectionsSection(companion, step) {
  const section = element("section", "block");
  section.setAttribute("aria-labelledby", "connections-title");
  const title = element("h2", "", "Connections");
  title.id = "connections-title";
  section.append(title);
  if (lobby.services.length === 0) {
    section.append(element("p", "muted", "This lobby has no service installed yet, so there is nothing to connect " + companion.displayName + " to."));
    return section;
  }
  const shown = shownConnections(companion);
  if (shown.length) {
    const list = element("ul", "rows");
    for (const connection of shown) list.append(connectionRow(companion, connection));
    section.append(list);
  } else {
    section.append(element("p", "muted", companion.displayName + " reaches no service yet."));
  }
  lobby.services.forEach((service, index) => section.append(grantPanel(companion, service, step === "connect" && index === 0)));
  return section;
}

function connectionRow(companion, connection) {
  const row = element("li", "row");
  const word = connectionWord(connection);
  const words = element("div", "row-words");
  words.append(element("strong", "", connection.label), element("span", "kind", serviceTitle(connection.service) + " at " + placeText(connection.address)));
  const shownWord = element("span", "word" + (word === "ready" ? " ok" : word === "revoked" ? " off" : " warn"), WORDS[word]);
  let action;
  if (word === "ready") {
    action = button("Revoke", "danger");
    confirmInPlace(action, "Revoke " + connection.label + "? Its tools go away for " + companion.displayName + " at the mind's next call.", "Revoke", () => revokeConnection(companion, connection));
  } else if (word === "sign_in_needed") {
    const credential = lobby.credentials.get(connection.credential);
    const again = button("Sign in again");
    again.dataset.key = "connection-" + connection.id;
    again.addEventListener("click", () => focusSignIn(credential ? credential.kind : ""));
    const revoke = button("Revoke", "quiet");
    revoke.dataset.key = "connection-revoke-" + connection.id;
    confirmInPlace(revoke, "Revoke " + connection.label + "? It does not come back when the account is signed in again.", "Revoke", () => revokeConnection(companion, connection));
    const actions = element("span", "actions");
    actions.append(again, revoke);
    row.append(words, shownWord, actions);
    return row;
  } else {
    const service = lobby.services.find(entry => entry.name === connection.service);
    const usable = service && service.accepts.map(kind => readyCredential(companion, kind)).find(Boolean);
    if (usable) {
      action = button("Open again");
      action.addEventListener("click", async () => {
        action.disabled = true;
        const granted = await grant(companion, usable, connection.service, connection.address, connection.label);
        action.disabled = false;
        if (granted) refresh(true, "connection-" + granted.id);
        else action.focus();
      });
    } else {
      const kind = service ? service.accepts[0] : "";
      action = button("Sign in");
      action.addEventListener("click", () => focusSignIn(kind));
    }
  }
  action.dataset.key = "connection-" + connection.id;
  row.append(words, shownWord, action);
  return row;
}

function focusSignIn(kind) {
  const row = document.getElementById("account-" + kind);
  const target = row && (row.querySelector("[data-sign-in]") || row.querySelector("input"));
  if (target) { row.scrollIntoView({ block: "center" }); target.focus(); }
}

async function revokeConnection(companion, connection) {
  const result = await call("/api/connections/" + encodeURIComponent(connection.id) + "/revoke", "POST", {});
  if (!result.ok) {
    note(sentence(result), true);
    if (result.status === 409) resynchronize();
    return;
  }
  lobby.connections.set(connection.id, result.body.connection);
  note(connection.label + " is revoked for " + companion.displayName + ". The lobby deleted what " + serviceTitle(connection.service) + " issued for it; the service may still take that until it expires there.");
  refresh(true, "connection-" + connection.id);
}

// Opens a connection; a refusal is said inside `form` when one asked.
async function grant(companion, credential, service, address, label, form) {
  const result = await call("/api/connections", "POST", { companion: companion.id, credential: credential.id, service, address, label });
  if (!result.ok) {
    if (result.status === 409) { note(sentence(result), true); resynchronize(); }
    else if (form) formSays(form, sentence(result));
    else note(sentence(result), true);
    return null;
  }
  const connection = result.body.connection;
  lobby.connections.set(connection.id, connection);
  const name = (lobby.companions.get(companion.id) || companion).displayName;
  const title = serviceTitle(service);
  const plain = [service, title].some(word => String(word).toLowerCase() === String(connection.label).toLowerCase());
  note(name + " can use " + title + (plain ? "" : " at " + connection.label) + ". Its tools appear at the mind's next call.");
  return connection;
}

// The address the sponsor typed, with its scheme: plain HTTP for one on
// this machine when none was typed, HTTPS for any other.
function withScheme(typed) {
  if (!typed || typed.includes("://")) return typed;
  const local = /^(localhost|127\.0\.0\.1|\[::1\])(:\d+)?\/?$/i.test(typed);
  return (local ? "http://" : "https://") + typed;
}

// A name suggested for a connection: the first word of the address's host,
// or the service's own name, made unique among the companion's.
function labelFrom(companion, service, address) {
  let host = "";
  try { host = new URL(address.includes("://") ? address : "https://" + address).hostname; } catch { host = ""; }
  let base = /^[a-z][a-z0-9-]*$/.test(host.split(".")[0] || "") && !/^\d/.test(host) ? host.split(".")[0] : service;
  base = monikerFrom(base) || service;
  const taken = new Set(connectionsOf(companion).filter(connection => connection.service === service && connection.state !== "revoked").map(connection => connection.label));
  if (!taken.has(base)) return base;
  for (let n = 2; ; n++) if (!taken.has(base + "-" + n)) return base + "-" + n;
}

function grantPanel(companion, service, primary) {
  const panel = element("div", "panel");
  const heading = element("h3", "", "Open a connection to " + service.title);
  panel.append(heading);
  const usable = service.accepts.map(kind => readyCredential(companion, kind)).filter(Boolean);
  const stale = service.accepts.map(kind => waitingCredential(companion, kind)).find(Boolean);
  if (usable.length === 0 && stale) {
    panel.append(element("p", "muted", "Sign " + stale.label + " in again above to open another connection to " + service.title + "."));
    return panel;
  }
  if (usable.length === 0) {
    const titles = service.accepts.map(kindTitle).join(" or ");
    panel.append(element("p", "muted", service.title + " takes " + (titles ? titles + " accounts" : "accounts this lobby cannot sign in") + ". Sign " + companion.displayName + " in above first."));
    return panel;
  }
  const form = element("form", "fields");
  form.setAttribute("aria-label", "Open a connection for " + companion.displayName + " to " + service.title);
  const key = "grant:" + companion.id + ":" + service.name;
  const draft = drafts.read(key) || {};
  const address = field("Address", draft.address, "Its address, such as https://" + service.name + ".example.test, or http://localhost:8080 for one on this machine.");
  const label = field("Name", draft.label, "What " + companion.displayName + " calls this " + service.title + " when it has more than one.");
  address.input.dataset.key = key + ":address";
  label.input.dataset.key = key + ":label";
  let labelEdited = !!draft.labelEdited;
  const save = () => drafts.write(key, { address: address.input.value, label: label.input.value, labelEdited });
  address.input.addEventListener("input", () => {
    if (!labelEdited) label.input.value = address.input.value.trim() ? labelFrom(companion, service.name, address.input.value.trim()) : "";
    save();
  });
  label.input.addEventListener("input", () => { labelEdited = true; save(); });
  form.append(address.wrapper, label.wrapper);
  let account = null;
  if (usable.length > 1) {
    const wrapper = element("div", "field");
    const caption = element("label", "", "Account");
    account = element("select");
    account.id = "field-" + (++fieldCount);
    caption.htmlFor = account.id;
    for (const credential of usable) {
      const option = element("option", "", credential.label + " (" + kindTitle(credential.kind) + ")");
      option.value = credential.id;
      account.append(option);
    }
    wrapper.append(caption, account);
    form.append(wrapper);
  }
  const open = element("button", primary ? "primary" : "", "Open connection");
  open.type = "submit";
  open.dataset.key = key + ":open";
  const actions = element("div", "actions");
  actions.append(open);
  form.append(actions);
  form.addEventListener("submit", async event => {
    event.preventDefault();
    const typed = address.input.value.trim();
    const where = withScheme(typed);
    const name = label.input.value.trim() || (where ? labelFrom(companion, service.name, where) : "");
    if (!where) { formSays(form, "Type where this " + service.title + " is first."); return; }
    const same = connectionsOf(companion).find(connection => connection.service === service.name && connection.state !== "revoked" && placeText(connection.address).replace(/\/$/, "") === placeText(where.toLowerCase()).replace(/\/$/, ""));
    if (same) { formSays(form, companion.displayName + " already reaches this " + service.title + " as " + same.label + "."); return; }
    const credential = account ? usable.find(entry => entry.id === account.value) : usable[0];
    await submitting(form, open, async () => {
      const granted = await grant(companion, credential, service.name, where, name, form);
      if (granted) { drafts.clear(key); editing = false; refresh(true, "connection-" + granted.id); }
    });
  });
  panel.append(form);
  return panel;
}

// ---------- names, portrait and removal ----------

function settings(companion) {
  const details = disclosure("settings", "Name and portrait", "settings-" + companion.id);
  const draft = drafts.read("rename:" + companion.id);
  if (draft) details.open = true;
  details.append(renameForm(companion, draft), portraitControls(companion), removal(companion));
  return details;
}

function renameForm(companion, draft) {
  const form = element("form", "fields");
  form.setAttribute("aria-label", "Rename " + companion.displayName);
  const key = "rename:" + companion.id;
  const name = field("Display name", draft ? draft.name : companion.displayName);
  const moniker = field("Moniker", draft ? draft.moniker : companion.moniker);
  name.input.dataset.key = key + ":name";
  moniker.input.dataset.key = key + ":moniker";
  const warning = element("p", "warning", "A host configuration that names this companion by its moniker stops working after the moniker changes. The block this lobby gives names it by its id and keeps working.");
  const update = () => {
    warning.hidden = moniker.input.value.trim() === companion.moniker;
    if (name.input.value === companion.displayName && moniker.input.value === companion.moniker) drafts.clear(key);
    else drafts.write(key, { name: name.input.value, moniker: moniker.input.value });
  };
  warning.hidden = moniker.input.value.trim() === companion.moniker;
  name.input.addEventListener("input", update);
  moniker.input.addEventListener("input", update);
  const save = element("button", "", "Save names");
  save.type = "submit";
  save.dataset.key = key + ":save";
  const discard = button("Discard changes", "quiet");
  discard.addEventListener("click", () => { drafts.clear(key); editing = false; refresh(true, key + ":name"); });
  const actions = element("div", "actions");
  actions.append(save, discard);
  form.append(name.wrapper, moniker.wrapper, warning, actions);
  form.addEventListener("submit", async event => {
    event.preventDefault();
    const change = { expectedRevision: companion.revision };
    if (name.input.value.trim() !== companion.displayName) change.displayName = name.input.value.trim();
    if (moniker.input.value.trim() !== companion.moniker) change.moniker = moniker.input.value.trim();
    if (!("displayName" in change) && !("moniker" in change)) { note("Nothing changed."); drafts.clear(key); return; }
    await submitting(form, save, async () => {
      const result = await call("/api/companions/" + encodeURIComponent(companion.id), "PATCH", change);
      if (!result.ok) {
        if (result.status === 409) { note(sentence(result), true); resynchronize(); }
        else formSays(form, sentence(result));
        return;
      }
      drafts.clear(key);
      keepNewer(result.body.companion);
      note("Saved.");
      editing = false;
      refresh(true, key + ":save");
    });
  });
  return form;
}

function portraitControls(companion) {
  const box = element("div", "fields");
  const choose = field("Portrait", "", "A PNG, JPEG, WebP or GIF image of at most 512 KiB.", { type: "file" });
  choose.input.accept = "image/png,image/jpeg,image/webp,image/gif";
  choose.input.dataset.key = "portrait-" + companion.id;
  choose.input.addEventListener("change", async () => {
    const file = choose.input.files && choose.input.files[0];
    if (!file) return;
    const updated = await uploadPortrait(companion, file, false);
    if (updated) { note("The portrait is changed."); editing = false; refresh(true, choose.input.dataset.key); }
  });
  box.append(choose.wrapper);
  if (companion.portrait) {
    const clear = button("Use initials instead", "quiet");
    clear.addEventListener("click", async () => {
      const result = await call("/api/companions/" + encodeURIComponent(companion.id) + "/portrait", "PUT", { expectedRevision: companion.revision, image: null });
      if (!result.ok) { note(sentence(result), true); if (result.status === 409) resynchronize(); return; }
      keepNewer(result.body.companion);
      note(companion.displayName + " shows its initials.");
      refresh(true, choose.input.dataset.key);
    });
    const actions = element("div", "actions");
    actions.append(clear);
    box.append(actions);
  }
  return box;
}

function removal(companion) {
  const box = element("div", "actions removal");
  const trigger = button("Remove " + companion.displayName, "danger");
  trigger.dataset.key = "remove-" + companion.id;
  const count = companion.attachments;
  const question = count
    ? "Remove " + companion.displayName + " and end its " + (count === 1 ? "attachment" : count + " attachments") + "? Its accounts are removed here too."
    : "Remove " + companion.displayName + "? Its accounts are removed here too.";
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
  box.append(trigger);
  return box;
}

// ---------- the journal ----------

function entryLine(entry) {
  const line = element("li");
  const time = element("time", "", timeText(entry.at));
  time.dateTime = new Date(entry.at).toISOString();
  time.title = new Date(entry.at).toLocaleString();
  const failed = !["done", "accepted", "pending"].includes(entry.outcome);
  line.append(element("span", "said" + (failed ? " failed" : ""), entry.sentence || ""), time);
  return line;
}

function journalQuery(state, before) {
  const query = ["finished=1"];
  if (state.scope) query.push("companion=" + encodeURIComponent(state.scope));
  if (!state.routine) query.push("routine=0");
  if (before) query.push("before=" + encodeURIComponent(before));
  return "/api/journal?" + query.join("&");
}

// A renewal that kept an account signed in as it should: kept in the
// journal, and shown on a companion's page only when the sponsor asks.
function routineEntry(entry) {
  return entry.command === "renew_credential" && entry.outcome === "done";
}

function journalArrived(entry) {
  // New entries join a list that shows the newest ones.
  const shown = journalView;
  if (!shown || !shown.live || entry.outcome === "pending") return;
  if (shown.scope && entry.companion !== shown.scope) return;
  if (!shown.routine && routineEntry(entry)) return;
  if (shown.entries.some(held => held.seq === entry.seq)) return;
  shown.entries.unshift(entry);
  if (shown.list && shown.list.isConnected) {
    shown.list.prepend(entryLine(entry));
    if (shown.empty) shown.empty.remove();
  }
}

function journalBlock(state) {
  const box = element("div");
  const list = element("ul", "journal");
  for (const entry of state.entries) list.append(entryLine(entry));
  state.list = list;
  box.append(list);
  state.empty = null;
  if (state.failed) box.append(element("p", "error", state.failed));
  else if (!state.loaded) box.append(element("p", "muted", "Reading the journal…"));
  else if (state.entries.length === 0) { state.empty = element("p", "muted", "Nothing has happened yet."); box.append(state.empty); }
  if (state.next) {
    const older = button("Older entries");
    older.dataset.key = "older";
    older.addEventListener("click", async () => {
      older.disabled = true;
      const result = await call(journalQuery(state, state.next));
      if (journalView !== state) return;
      older.disabled = false;
      if (!result.ok) {
        note(sentence(result), true);
        if (result.status === 409) { state.next = null; state.entries = []; state.loaded = false; loadJournal(state); }
        return;
      }
      state.entries.push(...result.body.entries);
      state.next = result.body.next;
      state.redraw();
      const again = view.querySelector('[data-key="older"]');
      if (again) again.focus({ preventScroll: true });
    });
    const actions = element("div", "actions");
    actions.append(older);
    box.append(actions);
  }
  return box;
}

async function loadJournal(state) {
  const result = await call(journalQuery(state, null));
  if (journalView !== state) return;
  if (!result.ok) { state.failed = sentence(result); state.loaded = true; state.redraw(); return; }
  // Entries that arrived while the page loaded and lie above it stay on top.
  const arrived = state.entries.filter(entry => entry.seq > result.body.upper);
  state.entries = arrived.concat(result.body.entries);
  state.next = result.body.next;
  state.loaded = true;
  state.redraw();
}

function journalSection(companion) {
  const section = element("section", "block");
  section.setAttribute("aria-labelledby", "journal-title");
  const title = element("h2", "", "Journal");
  title.id = "journal-title";
  const holder = element("div");
  if (!journalView || journalView.scope !== companion.id) {
    journalView = { scope: companion.id, routine: false, entries: [], next: null, live: true, list: null, loaded: false, failed: null, started: false };
  }
  const state = journalView;
  state.redraw = () => { if (holder.isConnected) holder.replaceChildren(journalBlock(state)); };
  holder.append(journalBlock(state));
  const toggle = button(state.routine ? "Hide routine entries" : "Show routine entries", "quiet");
  toggle.dataset.key = "routine-" + companion.id;
  toggle.setAttribute("aria-pressed", state.routine ? "true" : "false");
  toggle.addEventListener("click", () => {
    state.routine = !state.routine;
    toggle.textContent = state.routine ? "Hide routine entries" : "Show routine entries";
    toggle.setAttribute("aria-pressed", state.routine ? "true" : "false");
    state.entries = [];
    state.next = null;
    state.loaded = false;
    state.failed = null;
    state.redraw();
    loadJournal(state);
  });
  const toggleBox = element("div", "actions");
  toggleBox.append(toggle);
  section.append(title, element("p", "hint", "What happened with " + companion.displayName + ", newest first. Renewals that kept an account signed in are routine and wait behind Show routine entries. The journal holds no content and no secrets."), toggleBox, holder);
  return section;
}

function loadCompanionJournal() {
  const state = journalView;
  if (state && !state.started) { state.started = true; loadJournal(state); }
}

function renderJournalPage() {
  if (!journalView || journalView.scope !== null) {
    journalView = { scope: null, routine: true, entries: [], next: null, live: true, list: null, loaded: false, failed: null, started: false };
  }
  const state = journalView;
  const holder = element("div");
  state.redraw = () => { if (holder.isConnected) holder.replaceChildren(journalBlock(state)); };
  holder.append(journalBlock(state));
  view.replaceChildren(element("h1", "", "Journal"),
    element("p", "hint", "Who did what, as which companion, through which connection, and how it ended; newest first. The journal holds no content and no secrets."),
    holder);
  if (!state.started) { state.started = true; loadJournal(state); }
}

// ---------- start ----------

// The lobby loads once at start whatever the feed does; the feed's own reset
// loads it again, and only the latest load applies.
render();
startFeed();
resynchronize();
