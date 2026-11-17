import { INSTANCE_STRIDE } from "./noteStore";
import { FALLBACK, parseColor, type Rgba } from "../rack/palette";
import type { RollView } from "./view";

// The piano roll on WebGL2, in three passes: the grid (rows, black-key rows, step, beat and bar
// lines, the area past the pattern's end) as one full-screen triangle worked out per pixel; the
// notes as instanced quads in world space (tick, key), placed by the view uniforms; then the
// playhead and the selection box. A drag moves the selected notes in the vertex shader
// (`uDragDelta`, `uResizeDelta`), so the instance buffer is rebuilt only when the notes change.

/** The roll's colours, from the theme's custom properties. */
export interface RollPalette {
  inks: Rgba[];
  row: Rgba;
  rowBlack: Rgba;
  outside: Rgba;
  line: Rgba;
  beat: Rgba;
  bar: Rgba;
  select: Rgba;
  playhead: Rgba;
  box: Rgba;
}

const TOKENS = {
  row: "--roll-row",
  rowBlack: "--roll-row-black",
  outside: "--roll-outside",
  line: "--roll-line",
  beat: "--roll-beat",
  bar: "--roll-bar",
  select: "--roll-select",
  playhead: "--roll-playhead",
  box: "--roll-box",
} as const;

export function readRollPalette(root: Element): RollPalette {
  const style = getComputedStyle(root);
  const colour = (token: string): Rgba => parseColor(style.getPropertyValue(token)) ?? [...FALLBACK];
  const named = Object.fromEntries(Object.entries(TOKENS).map(([k, t]) => [k, colour(t)])) as Record<keyof typeof TOKENS, Rgba>;
  return { inks: [1, 2, 3, 4, 5, 6].map((n) => colour(`--ink-${n}`)), ...named };
}

/** Everything a frame needs besides the notes. */
export interface RollScene {
  view: RollView;
  dpr: number;
  /** The channel's ink, 0..5. */
  ink: number;
  beatTicks: number;
  barTicks: number;
  /** The pattern's end: the grid past it is dimmed. */
  endTick: number;
  /** The selection's drag so far, in ticks and keys. */
  drag: [number, number];
  /** The selection's resize so far, in ticks, and the shortest a note may get. */
  resize: number;
  minLength: number;
  playhead: number | null;
  /** The selection box, in CSS pixels. */
  box: { x0: number; y0: number; x1: number; y1: number } | null;
}

export interface RollRenderer {
  readonly kind: "webgl2" | "canvas2d";
  /** Takes the instance records (`buildInstances`). */
  setNotes(data: Float32Array, count: number): void;
  draw(scene: RollScene, palette: RollPalette): void;
  dispose(): void;
}

/** Note corner radius and the selection ring, in CSS pixels. */
const RADIUS = 2;
const RING = 1.5;
/** The fill of a note at velocity 0, as a share of its ink. */
const QUIET = 0.45;
const STEP = 240;

const GRID_VS = `#version 300 es
void main() {
  vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
  gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}`;

const GRID_FS = `#version 300 es
precision highp float;
uniform vec4 uView;      // scrollTick, topKey, tickPx, keyPx
uniform vec2 uCanvas;    // device pixels
uniform float uDpr;
uniform vec3 uTime;      // beat ticks, bar ticks, end tick
uniform vec4 uRow, uRowBlack, uOutside, uLine, uBeat, uBar;
out vec4 o;

float onLine(float tick, float period, float tickPx) {
  // One device pixel at the start of each period, when periods are at least 6 px apart.
  if (period * tickPx < 6.0) return 0.0;
  return mod(tick, period) * tickPx * uDpr < 1.0 ? 1.0 : 0.0;
}

void main() {
  vec2 css = vec2(gl_FragCoord.x, uCanvas.y - gl_FragCoord.y) / uDpr;
  float tick = uView.x + css.x / uView.z;
  float keyf = uView.y - css.y / uView.w;
  float key = floor(keyf);
  float pc = mod(key, 12.0);
  bool black = pc == 1.0 || pc == 3.0 || pc == 6.0 || pc == 8.0 || pc == 10.0;
  vec3 c = black ? uRowBlack.rgb : uRow.rgb;
  if (key < 0.0 || key > 127.0) c = uOutside.rgb;
  // Row edges: a line under every row, a stronger one under each C.
  float rowEdge = fract(keyf) * uView.w * uDpr < 1.0 ? 1.0 : 0.0;
  c = mix(c, pc == 0.0 ? uBeat.rgb : uLine.rgb, rowEdge * (pc == 0.0 ? uBeat.a : uLine.a));
  c = mix(c, uLine.rgb, onLine(tick, ${STEP.toFixed(1)}, uView.z) * uLine.a);
  c = mix(c, uBeat.rgb, onLine(tick, uTime.x, uView.z) * uBeat.a);
  c = mix(c, uBar.rgb, onLine(tick, uTime.y, uView.z) * uBar.a);
  if (tick >= uTime.z) c = mix(c, uOutside.rgb, 0.6);
  o = vec4(c, 1.0);
}`;

const NOTE_VS = `#version 300 es
layout(location = 0) in vec2 aCorner;
layout(location = 1) in vec4 aNote;   // tick, length, key, velocity
layout(location = 2) in float aFlags; // 1 selected, 2 slide
uniform vec4 uView;
uniform vec2 uSize;                    // CSS pixels
uniform vec2 uDragDelta;               // ticks, keys
uniform float uResizeDelta;
uniform float uMinLength;
out vec2 vLocal;
out vec2 vSize;
flat out float vVelocity;
flat out float vSelected;
void main() {
  bool selected = (int(aFlags + 0.5) & 1) != 0;
  float tick = aNote.x;
  float len = aNote.y;
  float key = aNote.z;
  if (selected) {
    tick += uDragDelta.x;
    key += uDragDelta.y;
    if (uResizeDelta != 0.0) len = max(uMinLength, len + uResizeDelta);
  }
  vec2 size = vec2(max(len * uView.z, 2.0), uView.w - 1.0);
  vec2 origin = vec2((tick - uView.x) * uView.z, (uView.y - key - 1.0) * uView.w);
  vLocal = aCorner * size;
  vSize = size;
  vVelocity = aNote.w;
  vSelected = selected ? 1.0 : 0.0;
  vec2 clip = (origin + vLocal) / uSize * 2.0 - 1.0;
  gl_Position = vec4(clip.x, -clip.y, 0.0, 1.0);
}`;

const NOTE_FS = `#version 300 es
precision highp float;
in vec2 vLocal;
in vec2 vSize;
flat in float vVelocity;
flat in float vSelected;
uniform vec4 uInk;
uniform vec4 uSelect;
uniform float uDpr;
out vec4 o;
float roundedBox(vec2 p, vec2 halfSize, float r) {
  vec2 q = abs(p) - halfSize + r;
  return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}
void main() {
  vec2 h = vSize * 0.5;
  float d = roundedBox(vLocal - h, h, min(${RADIUS.toFixed(1)}, min(h.x, h.y)));
  float cover = clamp(0.5 - d * uDpr, 0.0, 1.0);
  vec3 c = uInk.rgb * mix(${QUIET.toFixed(2)}, 1.0, clamp(vVelocity, 0.0, 1.0));
  if (vSelected > 0.5 && d > -${RING.toFixed(1)}) c = uSelect.rgb;
  o = vec4(c * cover, cover);
}`;

const RECT_VS = `#version 300 es
layout(location = 0) in vec2 aCorner;
uniform vec4 uRect;   // x, y, w, h in CSS pixels
uniform vec2 uSize;
void main() {
  vec2 clip = (uRect.xy + aCorner * uRect.zw) / uSize * 2.0 - 1.0;
  gl_Position = vec4(clip.x, -clip.y, 0.0, 1.0);
}`;

const RECT_FS = `#version 300 es
precision mediump float;
uniform vec4 uColor;
out vec4 o;
void main() { o = vec4(uColor.rgb * uColor.a, uColor.a); }`;

function program(gl: WebGL2RenderingContext, vs: string, fs: string): WebGLProgram | null {
  const p = gl.createProgram();
  const shaders: WebGLShader[] = [];
  for (const [type, src] of [
    [gl.VERTEX_SHADER, vs],
    [gl.FRAGMENT_SHADER, fs],
  ] as const) {
    const s = gl.createShader(type);
    if (!s) return null;
    gl.shaderSource(s, src);
    gl.compileShader(s);
    if (!gl.getShaderParameter(s, gl.COMPILE_STATUS) && !gl.isContextLost()) {
      console.error(`Piano roll shader: ${gl.getShaderInfoLog(s) ?? "did not compile"}`);
      return null;
    }
    gl.attachShader(p, s);
    shaders.push(s);
  }
  gl.linkProgram(p);
  for (const s of shaders) gl.deleteShader(s);
  if (!gl.getProgramParameter(p, gl.LINK_STATUS) && !gl.isContextLost()) {
    console.error(`Piano roll program: ${gl.getProgramInfoLog(p) ?? "did not link"}`);
    return null;
  }
  return p;
}

function locations<T extends string>(gl: WebGL2RenderingContext, p: WebGLProgram, names: readonly T[]) {
  return Object.fromEntries(names.map((n) => [n, gl.getUniformLocation(p, n)])) as Record<T, WebGLUniformLocation | null>;
}

/** A WebGL2 renderer for the canvas, or `null` without WebGL2 (or its shaders). */
export function createRollRenderer(canvas: HTMLCanvasElement): RollRenderer | null {
  const gl = canvas.getContext("webgl2", { antialias: false, alpha: false, premultipliedAlpha: true, powerPreference: "high-performance" });
  if (!gl) return null;
  const grid = program(gl, GRID_VS, GRID_FS);
  const notes = program(gl, NOTE_VS, NOTE_FS);
  const rect = program(gl, RECT_VS, RECT_FS);
  if (!grid || !notes || !rect) return null;
  const ug = locations(gl, grid, ["uView", "uCanvas", "uDpr", "uTime", "uRow", "uRowBlack", "uOutside", "uLine", "uBeat", "uBar"] as const);
  const un = locations(gl, notes, ["uView", "uSize", "uDragDelta", "uResizeDelta", "uMinLength", "uInk", "uSelect", "uDpr"] as const);
  const ur = locations(gl, rect, ["uRect", "uSize", "uColor"] as const);

  const quad = gl.createBuffer();
  gl.bindBuffer(gl.ARRAY_BUFFER, quad);
  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([0, 0, 1, 0, 0, 1, 1, 1]), gl.STATIC_DRAW);

  const instances = gl.createBuffer();
  const noteVao = gl.createVertexArray();
  gl.bindVertexArray(noteVao);
  gl.bindBuffer(gl.ARRAY_BUFFER, quad);
  gl.enableVertexAttribArray(0);
  gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);
  gl.bindBuffer(gl.ARRAY_BUFFER, instances);
  const stride = INSTANCE_STRIDE * 4;
  gl.enableVertexAttribArray(1);
  gl.vertexAttribPointer(1, 4, gl.FLOAT, false, stride, 0);
  gl.vertexAttribDivisor(1, 1);
  gl.enableVertexAttribArray(2);
  gl.vertexAttribPointer(2, 1, gl.FLOAT, false, stride, 16);
  gl.vertexAttribDivisor(2, 1);

  const rectVao = gl.createVertexArray();
  gl.bindVertexArray(rectVao);
  gl.bindBuffer(gl.ARRAY_BUFFER, quad);
  gl.enableVertexAttribArray(0);
  gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);
  gl.bindVertexArray(null);

  let count = 0;
  let capacity = 0;

  return {
    kind: "webgl2",
    setNotes(data, n) {
      if (gl.isContextLost()) return;
      gl.bindBuffer(gl.ARRAY_BUFFER, instances);
      if (data.length > capacity) {
        capacity = Math.max(data.length, capacity * 2);
        gl.bufferData(gl.ARRAY_BUFFER, capacity * 4, gl.DYNAMIC_DRAW);
      }
      gl.bufferSubData(gl.ARRAY_BUFFER, 0, data);
      count = n;
    },
    draw(s, p) {
      if (gl.isContextLost()) return;
      const v = s.view;
      gl.viewport(0, 0, canvas.width, canvas.height);
      gl.disable(gl.BLEND);

      gl.useProgram(grid);
      gl.uniform4f(ug.uView, v.scrollTick, v.topKey, v.tickPx, v.keyPx);
      gl.uniform2f(ug.uCanvas, canvas.width, canvas.height);
      gl.uniform1f(ug.uDpr, s.dpr);
      gl.uniform3f(ug.uTime, s.beatTicks, s.barTicks, s.endTick);
      gl.uniform4fv(ug.uRow, p.row);
      gl.uniform4fv(ug.uRowBlack, p.rowBlack);
      gl.uniform4fv(ug.uOutside, p.outside);
      gl.uniform4fv(ug.uLine, p.line);
      gl.uniform4fv(ug.uBeat, p.beat);
      gl.uniform4fv(ug.uBar, p.bar);
      gl.bindVertexArray(null);
      gl.drawArrays(gl.TRIANGLES, 0, 3);

      gl.enable(gl.BLEND);
      gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA);
      if (count > 0) {
        gl.useProgram(notes);
        gl.uniform4f(un.uView, v.scrollTick, v.topKey, v.tickPx, v.keyPx);
        gl.uniform2f(un.uSize, v.width, v.height);
        gl.uniform2f(un.uDragDelta, s.drag[0], s.drag[1]);
        gl.uniform1f(un.uResizeDelta, s.resize);
        gl.uniform1f(un.uMinLength, s.minLength);
        gl.uniform4fv(un.uInk, p.inks[s.ink] ?? FALLBACK);
        gl.uniform4fv(un.uSelect, p.select);
        gl.uniform1f(un.uDpr, s.dpr);
        gl.bindVertexArray(noteVao);
        gl.drawArraysInstanced(gl.TRIANGLE_STRIP, 0, 4, count);
      }

      gl.useProgram(rect);
      gl.uniform2f(ur.uSize, v.width, v.height);
      gl.bindVertexArray(rectVao);
      const fill = (x: number, y: number, w: number, h: number, c: Rgba) => {
        gl.uniform4f(ur.uRect, x, y, w, h);
        gl.uniform4fv(ur.uColor, c);
        gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
      };
      if (s.playhead !== null) fill((s.playhead - v.scrollTick) * v.tickPx - 1, 0, 2, v.height, p.playhead);
      if (s.box) {
        const x = Math.min(s.box.x0, s.box.x1);
        const y = Math.min(s.box.y0, s.box.y1);
        const w = Math.abs(s.box.x1 - s.box.x0);
        const h = Math.abs(s.box.y1 - s.box.y0);
        fill(x, y, w, h, p.box);
        fill(x, y, w, 1, p.select);
        fill(x, y + h - 1, w, 1, p.select);
        fill(x, y, 1, h, p.select);
        fill(x + w - 1, y, 1, h, p.select);
      }
      gl.bindVertexArray(null);
    },
    dispose() {
      gl.deleteBuffer(quad);
      gl.deleteBuffer(instances);
      gl.deleteVertexArray(noteVao);
      gl.deleteVertexArray(rectVao);
      gl.deleteProgram(grid);
      gl.deleteProgram(notes);
      gl.deleteProgram(rect);
    },
  };
}
