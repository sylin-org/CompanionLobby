'use strict';
(() => {
  const { scenes, renderer } = window.TangentAscii;
  const defaults = { scene: 'galaxy', color: '', intensity: 35, motion: true, mouseSpotlight: true };
  const reduced = matchMedia('(prefers-reduced-motion: reduce)');
  const forced = matchMedia('(forced-colors: active)');
  let server = { ...defaults }, participant, canManage = false, draft, frame, lastFrame = 0, time = 47, interval = 1000 / 15;
  let previewRenderer, previewWidth = 0, saving = false, pointerDirty = false;
  const pointer = { x: 0, y: 0, active: false };
  function sanitize(value) {
    const clean = {};
    if (value && typeof value === 'object') {
      if (scenes.some(s => s.id === value.scene) || value.scene === 'none') clean.scene = value.scene;
      if (typeof value.color === 'string' && /^(#[0-9a-f]{6})?$/i.test(value.color)) clean.color = value.color;
      if (Number.isFinite(value.intensity)) clean.intensity = Math.round(Math.max(0, Math.min(100, value.intensity)));
      if (typeof value.motion === 'boolean') clean.motion = value.motion;
      if (typeof value.mouseSpotlight === 'boolean') clean.mouseSpotlight = value.mouseSpotlight;
    }
    return clean;
  }
  // Appearance belongs to the server. Unsaved choices exist only while its editor is open.
  const settings = () => draft || server;
  const canvas = document.createElement('canvas'); canvas.id = 'ascii-atmosphere'; canvas.setAttribute('aria-hidden', 'true');
  document.body.prepend(canvas);
  const background = renderer(canvas);
  const panel = document.createElement('section'); panel.id = 'atmosphere-panel'; panel.hidden = true; panel.setAttribute('aria-labelledby', 'atmosphere-heading');
  panel.innerHTML = `<header class="atmosphere-heading"><div><p class="eyebrow">LIGHT, LETTERS & A LITTLE WONDER</p><h2 id="atmosphere-heading">Find the mood.</h2></div><button type="button" id="atmosphere-close" class="btn btn-quiet" aria-label="Close atmosphere settings">Close <span aria-hidden="true">×</span></button></header>
    <div class="atmosphere-stage"><canvas id="atmosphere-preview" aria-hidden="true"></canvas><div class="atmosphere-stage-caption"><span id="atmosphere-number"></span><h3 id="atmosphere-scene-name"></h3><p id="atmosphere-scene-note"></p></div><span class="atmosphere-material">MADE OF CHARACTERS. FULL OF LIFE.</span></div>
    <div class="atmosphere-scenes" role="group" aria-label="Choose an ASCII scene"></div>
    <div class="atmosphere-controls"><label class="atmosphere-intensity" for="atmosphere-intensity">Intensity <output id="atmosphere-intensity-value"></output><input id="atmosphere-intensity" type="range" min="0" max="100" step="1"></label><div class="atmosphere-color"><label><input id="atmosphere-palette" type="checkbox"> Scene colours</label><input id="atmosphere-color" type="color" aria-label="Custom background colour" value="#d4b57c"></div><label class="atmosphere-motion"><input id="atmosphere-motion" type="checkbox"> Gentle motion</label><label class="atmosphere-spotlight" title="Let nearby characters brighten as you move your mouse"><input id="atmosphere-spotlight" type="checkbox"> Mouse Spotlight</label></div>
    <p id="atmosphere-motion-note" class="hint" hidden>Your device prefers reduced motion. This scene will stay still.</p>
    <footer class="atmosphere-footer"><div class="atmosphere-secondary"><button type="button" id="atmosphere-reset" class="btn btn-quiet">Reset preview</button><button type="button" id="atmosphere-off" class="btn btn-quiet" aria-pressed="false">Background off</button></div><button type="button" id="atmosphere-save" class="btn btn-primary" hidden>Save atmosphere</button></footer>
    <p id="atmosphere-status" class="hint" role="status">Choose the atmosphere everyone sees here. Changes stay in preview until saved.</p>`;
  // The picker opens in place in Server settings, below the identity editor.
  const editor = document.querySelector('.server-identity-editor');
  if (editor) editor.after(panel); else document.body.append(panel);
  const $ = id => document.getElementById(id);
  const choices = panel.querySelector('.atmosphere-scenes');
  const thumbs = [];
  scenes.forEach((scene, i) => {
    const button = document.createElement('button'); button.type = 'button'; button.className = 'atmosphere-scene'; button.dataset.scene = scene.id; button.setAttribute('aria-pressed', 'false');
    const thumb = document.createElement('canvas'); thumb.setAttribute('aria-hidden', 'true');
    const number = document.createElement('span'); number.className = 'scene-number'; number.textContent = '0' + (i + 1);
    const name = document.createElement('span'); name.className = 'scene-name'; name.textContent = scene.name;
    button.append(thumb, number, name); button.addEventListener('click', () => change({ scene: scene.id })); choices.append(button);
    thumbs.push({ canvas: thumb, scene: scene.id, painter: renderer(thumb, true), width: 0 });
  });
  previewRenderer = renderer($('atmosphere-preview'));
  function change(patch) {
    if (!canManage || panel.hidden || saving) return;
    draft = { ...settings(), ...sanitize(patch) }; update(); restart();
    $('atmosphere-status').textContent = 'Previewing your changes. Save when it feels right.';
  }
  function moving() { const s = settings(); return s.motion && !reduced.matches && !forced.matches && !navigator.connection?.saveData && s.scene !== 'none' && s.intensity > 0; }
  function update() {
    const s = settings(), index = scenes.findIndex(scene => scene.id === s.scene), selected = scenes[index];
    document.body.classList.toggle('atmosphere-on', s.scene !== 'none' && s.intensity > 0);
    canvas.style.opacity = s.intensity > 0 ? '1' : '0';
    $('atmosphere-preview').style.opacity = s.intensity > 0 ? '1' : '0';
    canvas.dataset.scene = s.scene;
    $('atmosphere-scene-name').textContent = selected?.name || 'A little quiet.';
    $('atmosphere-scene-note').textContent = selected?.note || 'Only the conversations. You can bring the light back anytime.';
    $('atmosphere-number').textContent = selected ? '0' + (index + 1) + ' / 08' : 'STILL SPACE';
    choices.querySelectorAll('button').forEach(button => button.setAttribute('aria-pressed', String(button.dataset.scene === s.scene)));
    $('atmosphere-intensity').value = s.intensity; $('atmosphere-intensity-value').value = s.intensity + '%';
    $('atmosphere-palette').checked = !s.color; $('atmosphere-color').disabled = !s.color; if (s.color) $('atmosphere-color').value = s.color;
    $('atmosphere-spotlight').checked = s.mouseSpotlight;
    $('atmosphere-motion').checked = s.motion;
    $('atmosphere-motion-note').hidden = !reduced.matches && !navigator.connection?.saveData;
    $('atmosphere-motion-note').textContent = reduced.matches ? 'Your device prefers reduced motion. This scene will stay still.' : 'Data Saver is on. This scene will stay still.';
    $('atmosphere-save').hidden = !canManage; $('atmosphere-save').disabled = saving;
    $('atmosphere-off').setAttribute('aria-pressed', String(s.scene === 'none'));
    panel.querySelectorAll('.atmosphere-controls input, .atmosphere-scene, #atmosphere-reset, #atmosphere-off').forEach(control => { control.disabled = saving; });
    $('atmosphere-color').disabled = saving || !s.color;
  }
  function dimensions() {
    background.resize(innerWidth, innerHeight);
    if (!panel.hidden) {
      const box = $('atmosphere-preview').getBoundingClientRect();
      previewWidth = box.width; previewRenderer.resize(box.width, box.height);
      thumbs.forEach(thumb => { const width = thumb.canvas.clientWidth; if (width > 0 && width !== thumb.width) { thumb.width = width; thumb.painter.resize(width, thumb.canvas.clientHeight); thumb.painter.draw(thumb.scene, 47); } });
    }
  }
  function draw() {
    const s = settings(), start = performance.now();
    const spotlight = s.mouseSpotlight && pointer.active && !forced.matches && s.scene !== 'none' && s.intensity > 0
      ? { x: pointer.x, y: pointer.y, radius: Math.max(180, Math.min(420, Math.min(innerWidth, innerHeight) * .26)) } : undefined;
    background.draw(s.scene, time, s.color, { opacity: s.intensity / 100 * .85, spotlight });
    canvas.dataset.spotlight = spotlight ? 'active' : 'off';
    if (!panel.hidden && previewWidth > 0) {
      const box = $('atmosphere-preview').getBoundingClientRect();
      const inside = spotlight && pointer.x >= box.left && pointer.x <= box.right && pointer.y >= box.top && pointer.y <= box.bottom;
      previewRenderer.draw(s.scene, time, s.color, { opacity: Math.sqrt(s.intensity / 100), spotlight: inside
        ? { x: pointer.x - box.left, y: pointer.y - box.top, radius: Math.max(85, Math.min(180, box.width * .23)) } : undefined });
    }
    // Slow devices keep the same detailed composition and render fewer frames.
    interval = performance.now() - start > 28 ? 1000 / 10 : 1000 / 15;
  }
  function tick(now) {
    if (document.hidden || !moving() && !pointerDirty) { frame = undefined; return; }
    if (now - lastFrame >= interval) {
      if (moving()) time += Math.min((now - lastFrame) / 1000, .15);
      lastFrame = now; draw(); pointerDirty = false;
    }
    frame = moving() || pointerDirty ? requestAnimationFrame(tick) : undefined;
  }
  function restart() {
    cancelAnimationFrame(frame); frame = undefined; lastFrame = performance.now();
    canvas.dataset.motion = moving() && !document.hidden ? 'animated' : 'still';
    if (document.hidden) { pointer.active = false; pointerDirty = false; return; }
    draw(); pointerDirty = false; if (moving()) frame = requestAnimationFrame(tick);
  }
  function close() {
    if (panel.hidden) return;
    panel.hidden = true; $('server-atmosphere-open')?.setAttribute('aria-expanded', 'false'); resetWorking();
    if (location.pathname.replace(/\/$/, '') === '/settings') $('server-atmosphere-open')?.focus();
  }
  $('atmosphere-close').addEventListener('click', close);
  panel.addEventListener('keydown', event => { if (event.key === 'Escape') close(); });
  // An unsaved preview never follows the owner out of Server settings.
  window.addEventListener('tangent:route', () => { if (location.pathname.replace(/\/$/, '') !== '/settings') close(); });
  $('atmosphere-motion').addEventListener('change', event => change({ motion: event.target.checked }));
  $('atmosphere-spotlight').addEventListener('change', event => change({ mouseSpotlight: event.target.checked }));
  $('atmosphere-intensity').addEventListener('input', event => change({ intensity: Number(event.target.value) }));
  $('atmosphere-palette').addEventListener('change', event => change({ color: event.target.checked ? '' : $('atmosphere-color').value }));
  $('atmosphere-color').addEventListener('input', event => change({ color: event.target.value }));
  $('atmosphere-off').addEventListener('click', () => change({ scene: 'none' }));
  $('atmosphere-reset').addEventListener('click', () => { draft = { ...server }; update(); restart(); $('atmosphere-status').textContent = 'Preview restored to the saved atmosphere.'; });
  $('atmosphere-save').addEventListener('click', async () => {
    if (!canManage || saving) return;
    const actor = participant?.participantRef, s = settings(); saving = true; update();
    try {
      const response = await fetch('/api/server', { method: 'PATCH', credentials: 'same-origin', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ backgroundScene: s.scene, backgroundColor: s.color, backgroundIntensity: s.intensity, backgroundMotion: s.motion, backgroundMouseSpotlight: s.mouseSpotlight }) });
      const result = await response.json().catch(() => ({}));
      if (!response.ok) throw new Error(result.error || result.reason || 'The atmosphere could not be saved. Try again.');
      if (participant?.participantRef !== actor) return;
      server = fromServer(result); draft = panel.hidden ? undefined : { ...server }; update(); restart();
      $('atmosphere-status').textContent = 'A new atmosphere. Everyone arriving here will see it.';
    } catch (error) { $('atmosphere-status').textContent = error.message; }
    finally { saving = false; update(); }
  });
  function fromServer(value) {
    return { ...defaults, ...sanitize({ scene: value?.backgroundScene, color: value?.backgroundColor, intensity: value?.backgroundIntensity, motion: value?.backgroundMotion, mouseSpotlight: value?.backgroundMouseSpotlight }) };
  }
  function resetWorking() { draft = undefined; if (panel.hidden) previewWidth = 0; update(); restart(); }
  window.TangentAtmosphere = {
    configure(value, actor) {
      const changedActor = participant?.participantRef !== actor?.participantRef;
      server = fromServer(value); participant = actor; canManage = value?.canManage === true && !!actor?.participantRef;
      if (changedActor || !canManage) { draft = undefined; close(); }
      update(); restart();
    },
    open() {
      if (!canManage || location.pathname.replace(/\/$/, '') !== '/settings') return;
      if (!panel.hidden) { panel.scrollIntoView({ block: 'nearest' }); return; }
      draft = { ...server }; panel.hidden = false; $('server-atmosphere-open')?.setAttribute('aria-expanded', 'true');
      update(); dimensions(); restart(); panel.scrollIntoView({ block: 'nearest' });
      (choices.querySelector('[aria-pressed="true"]') || choices.querySelector('button'))?.focus();
      $('atmosphere-status').textContent = 'Choose the atmosphere everyone sees here. Changes stay in preview until saved.';
    },
    resetWorking
  };
  function pointerFrame() {
    if (document.hidden) return;
    pointerDirty = true;
    if (frame === undefined) frame = requestAnimationFrame(tick);
  }
  function clearPointer() {
    if (!pointer.active) return;
    pointer.active = false; pointerFrame();
  }
  // Mouse coordinates remain in this page. Touch does not acquire a sticky spotlight.
  document.addEventListener('pointermove', event => {
    if (event.pointerType !== 'mouse') { clearPointer(); return; }
    const s = settings();
    if (!s.mouseSpotlight || s.scene === 'none' || s.intensity === 0 || forced.matches) return;
    pointer.x = event.clientX; pointer.y = event.clientY; pointer.active = true; pointerFrame();
  }, { passive: true });
  document.addEventListener('pointerout', event => { if (event.relatedTarget === null) clearPointer(); });
  addEventListener('blur', clearPointer);
  addEventListener('resize', () => { dimensions(); restart(); }, { passive: true });
  document.addEventListener('visibilitychange', restart);
  reduced.addEventListener('change', () => { update(); restart(); });
  forced.addEventListener('change', () => { update(); restart(); });
  dimensions(); update(); restart();
})();
