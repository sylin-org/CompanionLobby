'use strict';
// One bounded glyph grid. Scenes paint light into it; the renderer draws ASCII from an atlas.
(() => {
  const TAU = Math.PI * 2;
  const clamp = (x, a = 0, b = 1) => Math.max(a, Math.min(b, x));
  const fract = x => x - Math.floor(x);
  const hash = n => fract(Math.sin(n * 127.1 + 311.7) * 43758.5453);
  const scenes = [
    { id: 'galaxy', name: 'Spiral Galaxy', note: 'A thousand small lights, finding their way home.', colors: ['#b4a9ee', '#7bc7cf', '#ffe0aa'] },
    { id: 'synapses', name: 'Synapses', note: 'An idea travels. Another answers.', colors: ['#608ab7', '#60d8cb', '#d2f8e0'] },
    { id: 'aurora', name: 'Aurora', note: 'Curtains of light across a midnight sky.', colors: ['#8e82da', '#4dcbb1', '#c1f3d1'] },
    { id: 'tides', name: 'Tidal Lines', note: 'Slow currents. Nothing stays quite the same.', colors: ['#648ab6', '#65cbd5', '#c2e8de'] },
    { id: 'orrery', name: 'Orrery', note: 'Little worlds, quietly keeping one another company.', colors: ['#9b9bc9', '#d2aa79', '#ffe5b5'] },
    { id: 'mycelium', name: 'Mycelium', note: 'Something is growing between us.', colors: ['#7b9c8f', '#b3c795', '#e4cca0'] },
    { id: 'rain', name: 'Silver Rain', note: 'The world outside. A warm place within.', colors: ['#777fae', '#aaa5d1', '#dbe3ed'] },
    { id: 'nebula', name: 'Nebula', note: 'Clouds of possibility, before they become stars.', colors: ['#7c99d8', '#c390bb', '#e9c4b0'] }
  ];
  const noiseTable = Float32Array.from({ length: 4096 }, (_, i) => hash(i + 219));
  function noise(x, y) {
    const ix = Math.floor(x), iy = Math.floor(y), fx = fract(x), fy = fract(y);
    const sx = fx * fx * (3 - 2 * fx), sy = fy * fy * (3 - 2 * fy);
    const a = noiseTable[(ix & 63) + ((iy & 63) << 6)], b = noiseTable[((ix + 1) & 63) + ((iy & 63) << 6)];
    const c = noiseTable[(ix & 63) + (((iy + 1) & 63) << 6)], d = noiseTable[((ix + 1) & 63) + (((iy + 1) & 63) << 6)];
    return (a + (b - a) * sx) * (1 - sy) + (c + (d - c) * sx) * sy;
  }
  function graph(count) {
    const nodes = Array.from({ length: count }, (_, i) => ({ x: .04 + hash(i * 4 + 1) * .92, y: .05 + hash(i * 4 + 2) * .9 }));
    const edges = [];
    nodes.forEach((p, i) => {
      const near = nodes.map((q, j) => ({ j, d: Math.hypot(p.x - q.x, p.y - q.y) })).filter(n => n.j !== i).sort((a, b) => a.d - b.d).slice(0, 3);
      near.forEach(({ j }) => { if (j > i) edges.push([i, j]); });
    });
    return { nodes, edges };
  }
  const networks = [graph(18), graph(28), graph(40), graph(55)];
  const roots = [];
  function branch(x, y, angle, length, depth, seed, age) {
    if (!depth) return;
    const bend = (hash(seed) - .5) * .45;
    const ex = x + Math.cos(angle + bend) * length, ey = y + Math.sin(angle + bend) * length;
    roots.push({ x, y, ex, ey, bend, age, seed, depth });
    branch(ex, ey, angle - .36 - hash(seed + 7) * .3, length * .74, depth - 1, seed + 23, age + length);
    branch(ex, ey, angle + .35 + hash(seed + 8) * .3, length * .73, depth - 1, seed + 51, age + length);
  }
  branch(.02, .98, -.74, .27, 6, 2, 0);
  branch(.98, .04, 2.38, .25, 6, 71, 0);
  class Field {
    constructor(columns, rows, aspect) {
      this.w = columns; this.h = rows; this.aspect = aspect;
      this.detail = Math.min(3, Math.floor(columns * rows / 8000));
      this.light = new Float32Array(columns * rows); this.tint = new Uint8Array(columns * rows); this.glyph = new Uint8Array(columns * rows);
    }
    clear() { this.light.fill(0); this.tint.fill(0); this.glyph.fill(0); }
    point(x, y, value, color = 0, glyph = 0) {
      const px = x * this.w, py = y * this.h, ix = Math.floor(px), iy = Math.floor(py);
      // Bilinear coverage makes travel smooth without making letters themselves swim.
      for (let dy = 0; dy < 2; dy++) for (let dx = 0; dx < 2; dx++) {
        const xx = ix + dx, yy = iy + dy;
        if (xx < 0 || yy < 0 || xx >= this.w || yy >= this.h) continue;
        const v = value * (dx ? px - ix : 1 - (px - ix)) * (dy ? py - iy : 1 - (py - iy)), index = yy * this.w + xx;
        if (v > this.light[index]) { this.light[index] = v; this.tint[index] = color; this.glyph[index] = glyph; }
      }
    }
    halo(x, y, radius, value, color = 2) {
      const px = x * this.w, py = y * this.h;
      for (let j = -radius; j <= radius; j++) for (let i = -radius; i <= radius; i++) {
        const xx = Math.floor(px + i), yy = Math.floor(py + j);
        if (xx < 0 || yy < 0 || xx >= this.w || yy >= this.h) continue;
        const v = value * Math.exp(-(i * i + j * j * 2.1) / (radius * radius * .45)), index = yy * this.w + xx;
        if (v > this.light[index]) { this.light[index] = v; this.tint[index] = color; }
      }
    }
    stars(t, amount = 100) {
      amount = Math.round(amount * Math.sqrt(this.w * this.h / 7000));
      for (let n = 0; n < amount; n++) {
        const x = hash(n * 3 + 191), y = hash(n * 3 + 712);
        this.point(x, y, .15 + .2 * Math.pow(.5 + .5 * Math.sin(t * .15 + n), 3), n % 3, n % 7 === 0 ? 7 : 1);
      }
    }
  }
  function paint(field, id, t) {
    field.clear();
    const { w, h, aspect, light, tint, glyph } = field;
    if (id === 'none') return;
    if (id === 'synapses') {
      const network = networks[field.detail];
      const nodes = network.nodes.map((p, n) => ({ x: p.x + .012 * Math.sin(t * .055 + n), y: p.y + .009 * Math.cos(t * .045 + n * 2) }));
      network.edges.forEach(([a, b], edge) => {
        const p = nodes[a], q = nodes[b], count = Math.ceil(Math.hypot((p.x - q.x) * w, (p.y - q.y) * h) * 2);
        const pulse = fract(t * .075 + hash(edge));
        for (let k = 0; k <= count; k++) {
          const f = k / count, signal = Math.exp(-Math.pow((f - pulse) * 16, 2));
          field.point(p.x + (q.x - p.x) * f, p.y + (q.y - p.y) * f, .38 + signal * .9, signal > .3 ? 2 : 0, Math.abs(p.x - q.x) > Math.abs(p.y - q.y) ? 5 : 4);
        }
      });
      nodes.forEach((p, n) => { field.halo(p.x, p.y, 3, .24 + .15 * Math.sin(t * .5 + n), 1); field.point(p.x, p.y, 1.6, 2, 9); });
      return;
    }
    if (id === 'orrery') {
      field.stars(t, 110);
      const cx = .67, cy = .46, scale = Math.min(1.25, aspect);
      const ringCount = 5 + field.detail;
      for (let ring = 0; ring < ringCount; ring++) {
        const radius = .105 + ring * (.36 / (ringCount - 1)), tilt = -.7 + ring * (1.2 / ringCount);
        const orbit = a => {
          const x = Math.cos(a) * radius, y = Math.sin(a) * radius * (.27 + ring * .035);
          return { x: cx + (x * Math.cos(tilt) - y * Math.sin(tilt)) / scale, y: cy + x * Math.sin(tilt) + y * Math.cos(tilt) };
        };
        for (let k = 0; k < w * 7; k++) {
          const a = k / (w * 7) * TAU, p = orbit(a);
          field.point(p.x, p.y, .52 + .18 * Math.sin(a + ring), ring % 2, 1);
        }
        const a = t * (.11 / (1 + ring * .4)) + ring * 1.95, p = orbit(a);
        field.halo(p.x, p.y, 3, .6, 1); field.point(p.x, p.y, 2, 2, 9);
      }
      field.halo(cx, cy, 6, .7, 1); field.point(cx, cy, 2, 2, 7); return;
    }
    if (id === 'mycelium') {
      roots.forEach((p, n) => {
        const count = Math.ceil(Math.hypot((p.ex - p.x) * w, (p.ey - p.y) * h) * 2);
        for (let k = 0; k <= count; k++) {
          const f = k / count, bend = Math.sin(f * Math.PI) * p.bend * .1;
          const pulse = Math.pow(.5 + .5 * Math.sin((p.age + f * .18) * 18 - t * .65), 12);
          field.point(p.x + (p.ex - p.x) * f + bend, p.y + (p.ey - p.y) * f, .36 + pulse * .65, pulse > .3 ? 2 : 0, p.ex > p.x ? 6 : 8);
        }
        if (p.depth < 3) field.halo(p.ex, p.ey, 2, .13 + .11 * Math.sin(t * .2 + n), 1);
      }); return;
    }
    if (id === 'rain') {
      for (let drop = 0; drop < w * .55; drop++) {
        const seed = hash(drop + 33), x = hash(drop * 4 + 22), y = fract(hash(drop + 95) + t * (.018 + seed * .025));
        const length = 3 + seed * 9;
        for (let k = 0; k < length; k++) field.point(fract(x + y * .055), y - k / h, Math.pow(1 - k / length, 1.6) * (.3 + seed * .7), seed > .7 ? 2 : 0, k === 0 ? 2 : 4);
        if (y > .87) {
          const r = (y - .87) * .22;
          for (let n = 0; n < 50; n++) { const a = n / 50 * TAU; field.point(x + .05 + Math.cos(a) * r / aspect, .9 + Math.sin(a) * r * .22, (1 - (y - .87) / .13) * .22, 1, 1); }
        }
      } return;
    }
    for (let j = 0; j < h; j++) for (let i = 0; i < w; i++) {
      const u = i / w, v = j / h, index = j * w + i;
      let value = 0, color = 0, shape = 0;
      if (id === 'galaxy') {
        const dx = (u - .67) * Math.min(aspect, 1.9), dy = (v - .43);
        const x = dx * .94 + dy * .34, y = (-dx * .34 + dy * .94) / .57;
        const r = Math.hypot(x, y), a = Math.atan2(y, x);
        const arm = Math.pow(.5 + .5 * Math.cos(3 * a - 6.8 * Math.log(r + .055) - t * .065), 9);
        const dust = .5 + .5 * noise(x * 42 + t * .018, y * 42);
        value = .63 * arm * Math.exp(-r * 2.9) * dust + .65 * Math.exp(-r * 12) + .055 * Math.exp(-r * 2);
        color = r < .16 ? 2 : arm > .6 ? 0 : 1;
        if (hash(index + 841) > .982) { value += .25 * arm; shape = 7; }
      } else if (id === 'aurora') {
        for (let curtain = 0; curtain < 3; curtain++) {
          const line = .2 + curtain * .14 + .085 * Math.sin(u * 7 + t * .065 + curtain) + .04 * Math.sin(u * 18 - t * .045 + curtain * 3);
          const d = v - line, beam = .56 + .44 * Math.sin(u * 72 + Math.sin(u * 11 + t * .1) * 3 + curtain);
          const veil = Math.exp(-d * d / (d > 0 ? .0008 : .013)) * (.22 + .45 * beam);
          if (veil > value) { value = veil; color = curtain === 1 ? 0 : d > -.018 ? 2 : 1; }
        }
        value *= .55 + .45 * Math.sin(u * Math.PI); shape = value > .32 ? 4 : 2;
      } else if (id === 'tides') {
        const x = u * Math.min(aspect, 2), y = v;
        const elevation = y * 7 + .7 * Math.sin(x * 6 + t * .07) + .42 * Math.sin(x * 12 - y * 4 - t * .08) + .25 * Math.cos(y * 10 + x * 3 + t * .04);
        const ridge = Math.pow(.5 + .5 * Math.cos(elevation * 5), 18);
        value = ridge * (.24 + .26 * noise(x * 5 - t * .03, y * 7)) + .028;
        color = ridge > .75 ? 1 : 0; shape = ridge > .65 ? 10 : 1;
      } else if (id === 'nebula') {
        const x = u * Math.min(aspect, 2) * 3, y = v * 3;
        const warp = noise(x + t * .018, y - t * .012);
        const cloud = noise(x + warp * 1.3, y + t * .024) * .57 + noise(x * 2.1 - t * .021, y * 2.1) * .28 + noise(x * 4.4, y * 4.4 + t * .016) * .15;
        value = Math.pow(clamp((cloud - .28) * 1.9), 1.4) * .85;
        const cavity = Math.exp(-Math.pow((u - .45) * 2, 2) - Math.pow((v - .52) * 2, 2));
        value *= 1 - cavity * .55;
        color = cloud > .62 ? 2 : warp > .5 ? 1 : 0;
      }
      light[index] = value; tint[index] = color; glyph[index] = shape;
    }
    if (['galaxy', 'aurora', 'nebula'].includes(id)) field.stars(t, id === 'aurora' ? 45 : 100);
  }
  const characters = ' .:,|-/*\\o~+#@';
  function spotlightColor(hex) {
    const rgb = [1, 3, 5].map(start => parseInt(hex.slice(start, start + 2), 16));
    const mid = (Math.max(...rgb) + Math.min(...rgb)) / 2;
    return 'rgb(' + rgb.map(c => Math.round(clamp((c - mid) * 1.7 + mid * 1.18, 0, 255))).join(',') + ')';
  }
  function renderer(canvas, thumbnail = false) {
    const ctx = canvas.getContext('2d', { alpha: true });
    let field, atlas, cellW, cellH, pixelRatio, paletteKey, lastWidth, lastHeight, paintedScene, paintedTime;
    function resize(width, height) {
      if (width === lastWidth && height === lastHeight) return;
      lastWidth = width; lastHeight = height;
      // Constant-size glyphs add real detail as the viewport grows. Only ultrawide/4K
      // displays hit the 42k-cell budget; device pixel ratio affects sharpness, not density.
      cellW = thumbnail ? Math.max(4, width / 60) : Math.max(9, Math.sqrt(width * height / (42000 * 1.65))); cellH = cellW * 1.65;
      pixelRatio = Math.min(devicePixelRatio || 1, 1.5);
      canvas.width = Math.round(width * pixelRatio); canvas.height = Math.round(height * pixelRatio);
      field = new Field(Math.ceil(width / cellW), Math.ceil(height / cellH), width / height);
      if (!thumbnail) { canvas.dataset.columns = String(field.w); canvas.dataset.rows = String(field.h); canvas.dataset.detail = String(field.detail); }
      paletteKey = '';
      paintedScene = undefined;
    }
    function draw(scene, time, customColor = '', { opacity = 1, spotlight } = {}) {
      if (!field || !ctx) return;
      const definition = scenes.find(s => s.id === scene) || scenes[0];
      const basePalette = customColor ? [customColor, customColor, '#edf1ef'] : definition.colors;
      const palette = [...basePalette, ...basePalette.map(spotlightColor)];
      const key = palette.join(',');
      const aw = Math.ceil(cellW * pixelRatio), ah = Math.ceil(cellH * pixelRatio);
      if (key !== paletteKey) {
        paletteKey = key; atlas = document.createElement('canvas'); atlas.width = aw * characters.length; atlas.height = ah * 48;
        const ac = atlas.getContext('2d'); ac.font = `${Math.round(cellH * .83 * pixelRatio)}px ui-monospace, Consolas, monospace`; ac.textBaseline = 'middle'; ac.textAlign = 'center';
        for (let color = 0; color < 6; color++) for (let level = 0; level < 8; level++) {
          ac.fillStyle = palette[color]; ac.globalAlpha = (level + 1) / 8;
          for (let c = 1; c < characters.length; c++) ac.fillText(characters[c], c * aw + aw / 2, (color * 8 + level) * ah + ah / 2);
        }
      }
      // Pointer-only updates reuse the frozen scene, including when ambient motion is paused.
      if (paintedScene !== scene || paintedTime !== time) { paint(field, scene, time); paintedScene = scene; paintedTime = time; }
      ctx.clearRect(0, 0, canvas.width, canvas.height);
      const radiusSquared = spotlight ? spotlight.radius * spotlight.radius : 1;
      for (let i = 0; i < field.light.length; i++) {
        const light = clamp(field.light[i] * 1.65); if (light < .055) continue;
        const level = Math.min(7, Math.floor(light * 8));
        const glyph = field.glyph[i] || (light < .14 ? 1 : light < .3 ? 2 : light < .48 ? 3 : light < .7 ? 7 : light < .88 ? 11 : 12);
        const x = (i % field.w) * cellW, y = Math.floor(i / field.w) * cellH;
        ctx.globalAlpha = opacity;
        ctx.drawImage(atlas, glyph * aw, (field.tint[i] * 8 + level) * ah, aw, ah, x * pixelRatio, y * pixelRatio, aw, ah);
        if (spotlight) {
          const dx = x + cellW / 2 - spotlight.x, dy = y + cellH / 2 - spotlight.y;
          const falloff = clamp(1 - (dx * dx + dy * dy) / radiusSquared);
          if (falloff > 0) {
            ctx.globalAlpha = falloff * falloff * (3 - 2 * falloff) * .7 * Math.min(1, opacity / .25);
            ctx.drawImage(atlas, glyph * aw, ((field.tint[i] + 3) * 8 + Math.min(7, level + 2)) * ah, aw, ah, x * pixelRatio, y * pixelRatio, aw, ah);
          }
        }
      }
      ctx.globalAlpha = 1;
    }
    return { resize, draw };
  }
  window.TangentAscii = { scenes, renderer };
})();
