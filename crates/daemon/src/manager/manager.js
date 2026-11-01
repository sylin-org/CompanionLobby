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

// A service's own page may open this one naming itself, `/?service=<name>&address=<origin>`.
// That only fills in a form: nothing connects until the sponsor reads it and opens the connection.
function takeHanded() {
  const asked = new URLSearchParams(location.search);
  const service = asked.get("service");
  const address = asked.get("address");
  if (!service && !address) return;
  history.replaceState({}, "", location.pathname);
  if (service && address) drafts.write("handed", { service, address });
}
takeHanded();

// What a service handed over, while the lobby offers that service.
function handed(service) {
  const place = drafts.read("handed");
  if (!place) return null;
  if (service) return place.service === service ? place : null;
  return lobby.services.some(entry => entry.name === place.service) ? place : null;
}

// A name suggested for a connection, in the shape of a moniker: lowercase ASCII, single hyphens.
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
  companions: new Map(), credentials: new Map(), connections: new Map(),
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

// The kinds of account a companion could still be signed in with.
function unusedKinds(companion) {
  return lobby.kinds.filter(entry => !liveCredentials(companion).some(credential => credential.kind === entry.kind));
}

function connectionsOf(companion) {
  return [...lobby.connections.values()].filter(connection => connection.companion === companion.id);
}

// Whether a ready account was granted everything a service needs of it.
function grantsAll(credential, service) {
  return (service.needs || []).every(permission => (credential.permissions || []).includes(permission));
}

// The services of a ready account's kind that need more than it holds, so
// signing the same account in again would widen it.
function widenedBy(credential) {
  return lobby.services.filter(service => service.accepts.includes(credential.kind) && !grantsAll(credential, service));
}

// The titles of the services an account's ready connections reach, each once.
function reachedThrough(credential) {
  const titles = [];
  for (const connection of lobby.connections.values()) {
    if (connection.credential !== credential.id || connection.state !== "ready") continue;
    const title = serviceTitle(connection.service);
    if (!titles.includes(title)) titles.push(title);
  }
  return titles;
}

// Joins words as a sentence lists them: "A", "A and B", "A, B and C".
function listed(words) {
  return words.length < 2 ? words.join("") : words.slice(0, -1).join(", ") + " and " + words[words.length - 1];
}

// What signing a ready account in again does, in one or two sentences:
// what the companion can then do, and the connections that stay.
function widenSentence(companion, credential, services) {
  const name = companion.displayName;
  const staying = reachedThrough(credential);
  const stays = staying.length ? " " + listed(staying) + (staying.length === 1 ? " stays" : " stay") + " connected." : "";
  return "Sign " + name + " in again so " + name + " can " + listed(services.map(service => service.lets || "use " + service.title)) + "." + stays;
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
  if (lobby.kinds.some(entry => waitingCredential(companion, entry.kind))) return "sign-in-again";
  if (!lobby.kinds.some(entry => readyCredential(companion, entry.kind))) return "sign-in";
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
  else if (parts[0] === "sign-in" && parts.length === 1) renderSignIn();
  else if (parts[0] === "journal" && parts.length === 1) renderJournalPage();
  else if (parts[0] === "companion" && parts[1] && parts.length === 2) renderCompanion(parts[1]);
  else if (parts[0] === "companion" && parts.length === 3 && parts[2] === "attach") renderAttach(parts[1]);
  else { history.replaceState({}, "", "/"); renderLobby(); }
}

function notHere(text) {
  view.replaceChildren(link("/", "back", "All companions"), element("h1", "", "Not here"), element("p", "muted", text));
}

// ---------- the lobby ----------

function renderLobby() {
  const companions = [...lobby.companions.values()];
  const parts = [element("h1", "", "Companions")];
  const place = handed();
  if (place) {
    const title = lobby.services.find(entry => entry.name === place.service).title;
    parts.push(element("p", "muted", companions.length
      ? "Choose who connects to the " + title + " at " + placeText(place.address) + ", or sign in with another account. Its address is ready on their page."
      : "Sign in to connect to the " + title + " at " + placeText(place.address) + "."));
  }
  if (companions.length) {
    const list = element("ul", "cards");
    for (const companion of companions) list.append(card(companion));
    parts.push(list);
  } else {
    parts.push(element("p", "muted", "No companions yet."));
  }
  const signIn = link("/sign-in", "button primary", "Sign in");
  signIn.dataset.key = "sign-in";
  const actions = element("div", "actions");
  actions.append(signIn);
  parts.push(actions);
  view.replaceChildren(...parts);
}

// Signing in is how a companion comes to be: the account the provider signs
// in opens the companion it belongs to, or makes one of its own.
function renderSignIn() {
  const parts = [link("/", "back", "All companions"), element("h1", "", "Sign in"),
    element("p", "lede", "Sign in with the account a companion goes by. An account the lobby knows opens its companion; a new one makes a companion, named after the account, which you can change later.")];
  const place = handed();
  if (place) parts.push(element("p", "muted", "Then the connection to " + placeText(place.address) + " is ready for you to open."));
  if (lobby.kinds.length === 0) parts.push(element("p", "muted", "No kind of account is installed in this lobby."));
  else parts.push(signInChoices(null, true));
  view.replaceChildren(...parts);
}

// One way to sign in per kind of account: a button that goes straight to the
// provider's page, and a field for naming the account first. With no
// companion, the account says which one it is for.
function signInChoices(companion, primary, kinds) {
  const list = element("div", "sign-in-choices");
  (kinds || lobby.kinds).forEach((entry, index) => list.append(signInChoice(companion, entry, primary && index === 0)));
  return list;
}

function signInChoice(companion, entry, primary) {
  const form = element("form", "sign-in-choice");
  form.setAttribute("aria-label", "Sign in with " + entry.title);
  const key = "sign-in:" + (companion ? companion.id : "lobby") + ":" + entry.kind;
  const go = element("button", primary ? "primary" : "", "Sign in with " + entry.title);
  go.type = "submit";
  go.dataset.key = key + ":go";
  const named = disclosure("details", "Name the account first", key + ":named");
  const typed = drafts.read(key) || "";
  if (typed) named.open = true;
  const account = field(entry.title + " account", typed, entry.hint || "The account to sign in with.");
  account.input.dataset.key = key;
  account.input.addEventListener("input", () => drafts.write(key, account.input.value));
  named.append(account.wrapper);
  form.append(go, named);
  form.addEventListener("submit", async event => {
    event.preventDefault();
    await submitting(form, go, async () => {
      const onward = await startSignIn(companion, entry.kind, account.input.value.trim(), form);
      if (onward) { drafts.clear(key); location.assign(onward); }
      else { named.open = true; account.input.focus(); }
    });
  });
  return form;
}

// Starts a sign-in and answers where the browser goes for it, or says why not,
// inside `form` when one asked, and answers null.
async function startSignIn(companion, kind, hint, form) {
  const request = { kind, hint };
  if (companion) request.companion = companion.id;
  const result = await call("/api/sign-ins", "POST", request);
  if (result.ok) return result.body.url;
  if (result.status === 409) { note(sentence(result), true); resynchronize(); }
  else if (form) formSays(form, sentence(result));
  else note(sentence(result), true);
  return null;
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

// One account on a companion's card: Remove while it is signed in; while it
// waits for a sign-in, the way back, Sign in again, and Remove beside it.
function cardAccount(companion, credential) {
  const line = element("li", "card-account");
  const ready = credential.state === "ready";
  const said = element("span", ready ? "" : "warn", credential.label + " · " + kindTitle(credential.kind) + (ready ? "" : " · Sign in needed"));
  const remove = button("Remove", ready ? "danger" : "quiet");
  remove.dataset.key = "card-remove-" + credential.id;
  confirmInPlace(remove, removeQuestion(companion, credential), "Remove", () => removeCredential(companion, credential, "card-" + companion.id));
  if (ready) {
    line.append(said, remove);
    return line;
  }
  const back = button("Sign in again");
  back.dataset.key = "card-again-" + credential.id;
  back.addEventListener("click", () => signInAgain(companion, credential, back));
  const actions = element("span", "actions");
  actions.append(back, remove);
  line.append(said, actions);
  return line;
}

// Starts signing the same account in again and goes to the provider's page.
async function signInAgain(companion, credential, control) {
  control.disabled = true;
  const onward = await startSignIn(companion, credential.kind, "");
  control.disabled = false;
  if (onward) location.assign(onward);
  else control.focus();
}

function removeQuestion(companion, credential) {
  const derived = connectionsOf(companion).filter(connection => connection.credential === credential.id && connection.state !== "revoked").length;
  return "Remove " + credential.label + " from " + companion.displayName + "? It is revoked at its provider." + (derived ? " Its connections are revoked too." : "");
}

function readAsDataUrl(file) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(reader.result);
    reader.onerror = () => reject(reader.error);
    reader.readAsDataURL(file);
  });
}

// Sends a portrait; a refusal is said at once.
async function uploadPortrait(companion, file) {
  if (file.size > 512 * 1024) { note("That image is too large; a portrait is at most 512 KiB.", true); return null; }
  let image;
  try { image = await readAsDataUrl(file); } catch { note("That file could not be read.", true); return null; }
  const result = await call(companionPath(companion).replace("/companion/", "/api/companions/") + "/portrait", "PUT", { expectedRevision: companion.revision, image });
  if (!result.ok) { note(sentence(result), true); if (result.status === 409) resynchronize(); return null; }
  keepNewer(result.body.companion);
  return result.body.companion;
}

// ---------- first attachment: what just happened and what comes next ----------

function renderAttach(id) {
  const companion = lobby.companions.get(id);
  if (!companion) { notHere("No companion has that id; it may have been removed."); return; }
  const parts = [link(companionPath(companion), "back", companion.displayName)];
  const heading = element("div", "identity");
  heading.append(avatar(companion, "large"), element("h1", "", companion.displayName + (companion.attachments > 0 ? " is attached" : " is ready to attach")));
  parts.push(heading);
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
    const reaching = connectionsOf(companion).some(connection => connection.state === "ready");
    status.textContent = tidy("A mind is attached as " + companion.displayName + "." + (reaching ? "" : " Next, open a connection."));
    const next = link(companionPath(companion), "button primary", reaching ? "Back to " + companion.displayName : "Open a connection");
    next.dataset.key = "onward";
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
  const words = element("div", "identity-words");
  const name = element("h1");
  name.append(nameInPlace(companion, "displayName", "Display name"));
  const facts = element("p", "muted identity-facts");
  facts.append(nameInPlace(companion, "moniker", "Moniker"), element("span", "", "· " + attachedText(companion.attachments)));
  words.append(name, facts);
  head.append(portraitInPlace(companion), words);
  const tools = element("div", "head-tools");
  tools.append(link(companionPath(companion, "/attach"), "", "Host configuration"));
  view.replaceChildren(link("/", "back", "All companions"), head, tools,
    accountsSection(companion, step), connectionsSection(companion, step), journalSection(companion), removal(companion));
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
  const held = liveCredentials(companion);
  if (held.length) {
    const list = element("ul", "rows");
    for (const credential of held) list.append(accountRow(companion, credential, step === "sign-in-again"));
    section.append(list);
  } else {
    const removed = credentialsOf(companion).pop();
    section.append(element("p", "muted", removed
      ? removed.label + " was removed. Signing in with it again brings it back here."
      : companion.displayName + " holds no account yet."));
  }
  const unused = unusedKinds(companion);
  if (unused.length) {
    const add = disclosure("add-account" + (step === "sign-in" ? " first" : ""), "+ Add", "add-account-" + companion.id);
    add.id = "add-account";
    add.append(signInChoices(companion, false, unused));
    section.append(add);
  }
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

function accountRow(companion, credential, again) {
  const row = element("li", "row");
  row.id = "account-" + credential.kind;
  const entry = lobby.kinds.find(kind => kind.kind === credential.kind) || { title: credential.kind };
  const { words } = accountWords(credential, entry);
  if (credential.state === "ready") {
    const remove = button("Remove", "danger");
    remove.dataset.key = "remove-" + credential.id;
    remove.dataset.signIn = "";
    confirmInPlace(remove, removeQuestion(companion, credential), "Remove", () => removeCredential(companion, credential));
    row.append(words, element("span", "word ok", "Signed in"), remove);
    return row;
  }
  // The way back is signing the same account in again; removing it instead
  // sits beside it, quieter.
  const instead = button("Remove", "quiet");
  instead.dataset.key = "remove-" + credential.id;
  confirmInPlace(instead, removeQuestion(companion, credential), "Remove", () => removeCredential(companion, credential));
  const signIn = button("Sign in again", again ? "primary" : "");
  signIn.dataset.key = "again-" + credential.id;
  signIn.dataset.signIn = "";
  signIn.addEventListener("click", () => signInAgain(companion, credential, signIn));
  const actions = element("span", "actions");
  actions.append(signIn, instead);
  row.append(words, element("span", "word warn", "Sign in needed"), actions);
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
  sent_last_known: title => " It waited for a sign-in, so " + title + " was sent the last key the lobby held, which may already have been replaced: the lobby cannot tell whether it is revoked there. To be sure, open the account's own settings at " + title + ", find Companion Lobby among the apps it allows, and remove it there.",
  not_asked: title => " The lobby held no key for it, so " + title + " was not asked to revoke it; it ends there when it expires.",
};

async function removeCredential(companion, credential, focusKey) {
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
  note(credential.label + " is removed from " + companion.displayName + "." + said(kindTitle(credential.kind)));
  refresh(true, focusKey || "remove-" + companion.id);
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
  const place = handed();
  const first = place ? place.service : lobby.services[0].name;
  lobby.services.map(service => grantPanel(companion, service, step === "connect" && service.name === first)).filter(Boolean).forEach(panel => section.append(panel));
  return section;
}

function connectionRow(companion, connection) {
  const row = element("li", "row");
  const word = connectionWord(connection);
  const words = element("div", "row-words");
  const service = lobby.services.find(entry => entry.name === connection.service);
  const through = lobby.credentials.get(connection.credential);
  const where = service && service.home && through ? " through " + through.label : " at " + placeText(connection.address);
  words.append(element("strong", "", connection.label), element("span", "kind", serviceTitle(connection.service) + where));
  const shownWord = element("span", "word" + (word === "ready" ? " ok" : word === "revoked" ? " off" : " warn"), WORDS[word]);
  let action;
  if (word === "ready") {
    action = button("Revoke", "danger");
    confirmInPlace(action, "Revoke " + connection.label + "? Its tools go away for " + companion.displayName + " at the mind's next call.", "Revoke", () => revokeConnection(companion, connection));
  } else if (word === "sign_in_needed") {
    const credential = lobby.credentials.get(connection.credential);
    const again = button("Sign in again");
    again.dataset.key = "connection-" + connection.id;
    again.addEventListener("click", () => focusSignIn(companion, credential ? credential.kind : ""));
    const revoke = button("Revoke", "quiet");
    revoke.dataset.key = "connection-revoke-" + connection.id;
    confirmInPlace(revoke, "Revoke " + connection.label + "? It does not come back when the account is signed in again.", "Revoke", () => revokeConnection(companion, connection));
    const actions = element("span", "actions");
    actions.append(again, revoke);
    row.append(words, shownWord, actions);
    return row;
  } else {
    const usable = service && service.accepts.map(kind => readyCredential(companion, kind)).find(credential => credential && grantsAll(credential, service));
    if (usable) {
      action = button("Open again");
      action.addEventListener("click", async () => {
        action.disabled = true;
        const granted = await grant(companion, usable, connection.service, service.home ? "" : connection.address, connection.label);
        action.disabled = false;
        if (granted) refresh(true, "connection-" + granted.id);
        else action.focus();
      });
    } else {
      const kind = service ? service.accepts[0] : "";
      action = button("Sign in");
      action.addEventListener("click", () => focusSignIn(companion, kind));
    }
  }
  action.dataset.key = "connection-" + connection.id;
  row.append(words, shownWord, action);
  return row;
}

// Leads to where the account of `kind` signs in: its row, or + Add, opened.
function focusSignIn(companion, kind) {
  const row = document.getElementById("account-" + kind);
  const add = document.getElementById("add-account");
  if (add) add.open = true;
  const target = row ? row.querySelector("[data-sign-in]") : add && add.querySelector("button");
  if (target) { target.scrollIntoView({ block: "center" }); target.focus(); }
}

async function revokeConnection(companion, connection) {
  const result = await call("/api/connections/" + encodeURIComponent(connection.id) + "/revoke", "POST", {});
  if (!result.ok) {
    note(sentence(result), true);
    if (result.status === 409) resynchronize();
    return;
  }
  lobby.connections.set(connection.id, result.body.connection);
  const service = serviceTitle(connection.service);
  note(connection.label + " is revoked for " + companion.displayName + ". The lobby deleted what " + service + " issued for it; " + service + " may still honor that until it expires there.");
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
    panel.append(element("p", "muted", "Sign " + stale.label + " in again under Accounts to open another connection to " + service.title + "."));
    return panel;
  }
  if (usable.length === 0) {
    const titles = service.accepts.map(kindTitle).join(" or ");
    panel.append(element("p", "muted", service.title + " takes " + (titles ? titles + " accounts" : "accounts this lobby cannot sign in") + ". Add one under Accounts first."));
    return panel;
  }
  if (service.home) return homePanel(companion, service, usable, panel, primary);
  const form = element("form", "fields");
  form.setAttribute("aria-label", "Open a connection for " + companion.displayName + " to " + service.title);
  const key = "grant:" + companion.id + ":" + service.name;
  const place = handed(service.name);
  const draft = drafts.read(key) || (place ? { address: place.address, label: labelFrom(companion, service.name, place.address) } : {});
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
      if (granted) {
        drafts.clear(key);
        if (place && placeText(place.address) === placeText(where)) drafts.clear("handed");
        editing = false;
        refresh(true, "connection-" + granted.id);
      }
    });
  });
  panel.append(form);
  return panel;
}

// A service that lives where the account lives: no address to type, one
// connection per account, so no panel once it stands, and an account
// signed in before the service needed more of it is offered one thing,
// signing it in again.
function homePanel(companion, service, usable, panel, primary) {
  const credential = usable.find(entry => grantsAll(entry, service)) || usable[0];
  const standing = connectionsOf(companion).find(connection => connection.service === service.name && connection.credential === credential.id && connection.state !== "revoked");
  if (standing) return null;
  const form = element("form", "fields");
  const key = "grant:" + companion.id + ":" + service.name;
  const actions = element("div", "actions");
  if (!grantsAll(credential, service)) {
    form.setAttribute("aria-label", "Sign " + companion.displayName + " in again for " + service.title);
    form.append(element("p", "", widenSentence(companion, credential, [service])));
    const again = element("button", primary ? "primary" : "", "Sign " + companion.displayName + " in again");
    again.type = "submit";
    again.dataset.key = key + ":again";
    actions.append(again);
    form.append(actions);
    form.addEventListener("submit", async event => {
      event.preventDefault();
      await submitting(form, again, async () => {
        const onward = await startSignIn(companion, credential.kind, "", form);
        if (!onward) return;
        editing = false;
        location.assign(onward);
      });
    });
    panel.append(form);
    return panel;
  }
  form.setAttribute("aria-label", "Open a connection for " + companion.displayName + " to " + service.title);
  form.append(element("p", "muted", service.title + " lives where the account lives, so " + companion.displayName + " reaches it as " + credential.label + ". There is no address to type."));
  const open = element("button", primary ? "primary" : "", "Open connection");
  open.type = "submit";
  open.dataset.key = key + ":open";
  actions.append(open);
  form.append(actions);
  form.addEventListener("submit", async event => {
    event.preventDefault();
    await submitting(form, open, async () => {
      const granted = await grant(companion, credential, service.name, "", service.name, form);
      if (granted) { editing = false; refresh(true, "connection-" + granted.id); }
    });
  });
  panel.append(form);
  return panel;
}

// ---------- names, portrait and removal ----------

// A name edited where it stands: it reads as text and takes the change when
// the sponsor leaves it or presses Enter; Escape puts it back.
function nameInPlace(companion, what, label) {
  const shown = what === "displayName" ? companion.displayName : companion.moniker;
  const form = element("form", "in-place");
  const input = element("input");
  input.value = shown;
  input.autocomplete = "off";
  input.spellcheck = false;
  input.size = shown.length + 1;
  input.setAttribute("aria-label", label + " of " + companion.displayName);
  input.dataset.key = what + "-" + companion.id;
  form.append(input);
  input.addEventListener("keydown", event => {
    if (event.key !== "Escape") return;
    input.value = shown;
    input.blur();
  });
  input.addEventListener("input", () => {
    input.size = input.value.length + 1;
    const moving = what === "moniker" && input.value.trim() !== shown;
    form.querySelector(".warning")?.remove();
    if (moving) form.append(element("p", "warning", "A host configuration that names this companion by its moniker stops working after the moniker changes. The block this lobby gives names it by its id and keeps working."));
  });
  input.addEventListener("change", async () => {
    const value = input.value.trim();
    if (!value || value === shown) { input.value = shown; form.querySelector(".warning")?.remove(); return; }
    form.dataset.busy = "1";
    const result = await call("/api/companions/" + encodeURIComponent(companion.id), "PATCH", { expectedRevision: companion.revision, [what]: value });
    delete form.dataset.busy;
    if (!result.ok) {
      if (result.status === 409) { note(sentence(result), true); resynchronize(); }
      else formSays(form, sentence(result));
      return;
    }
    keepNewer(result.body.companion);
    note("Saved.");
    editing = false;
    refresh(true);
  });
  form.addEventListener("submit", event => { event.preventDefault(); input.blur(); });
  return form;
}

// The portrait, changed by choosing a file right on it.
function portraitInPlace(companion) {
  const holder = element("div", "portrait-in-place");
  const pick = element("label", "portrait-pick");
  pick.title = "Choose a PNG, JPEG, WebP or GIF image of at most 512 KiB.";
  const input = element("input");
  input.type = "file";
  input.accept = "image/png,image/jpeg,image/webp,image/gif";
  input.setAttribute("aria-label", "Portrait of " + companion.displayName);
  input.dataset.key = "portrait-" + companion.id;
  input.addEventListener("change", async () => {
    const file = input.files && input.files[0];
    if (!file) return;
    const updated = await uploadPortrait(companion, file);
    if (updated) { note("The portrait is changed."); editing = false; refresh(true, input.dataset.key); }
  });
  pick.append(avatar(companion, "large"), input);
  holder.append(pick);
  if (companion.portrait) {
    const clear = button("Use initials", "quiet small");
    clear.addEventListener("click", async () => {
      const result = await call("/api/companions/" + encodeURIComponent(companion.id) + "/portrait", "PUT", { expectedRevision: companion.revision, image: null });
      if (!result.ok) { note(sentence(result), true); if (result.status === 409) resynchronize(); return; }
      keepNewer(result.body.companion);
      note(companion.displayName + " shows its initials.");
      refresh(true, input.dataset.key);
    });
    holder.append(clear);
  }
  return holder;
}

function removal(companion) {
  const box = element("div", "actions removal");
  const trigger = button("Remove " + companion.displayName, "quiet");
  trigger.dataset.key = "remove-" + companion.id;
  const count = companion.attachments;
  const question = count
    ? "Remove " + companion.displayName + " and end its " + (count === 1 ? "attachment" : count + " attachments") + "? Its accounts are removed too, and free to sign in again as a new companion."
    : "Remove " + companion.displayName + "? Its accounts are removed too, and free to sign in again as a new companion.";
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
  // One control with one name: pressed while the routine entries show.
  const toggle = button("Show routine entries", "quiet");
  toggle.dataset.key = "routine-" + companion.id;
  toggle.setAttribute("aria-pressed", state.routine ? "true" : "false");
  toggle.addEventListener("click", () => {
    state.routine = !state.routine;
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
