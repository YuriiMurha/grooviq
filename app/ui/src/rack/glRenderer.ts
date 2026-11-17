import { EDGE, STRIDE, type GridLayout } from "./gridModel";
import type { GridRenderer, Palette } from "./palette";

// The step grid on WebGL2: one unit quad, instanced once per cell, drawn in two passes over the
// same instance buffer. Pass 1 draws each lit cell's second ink, offset by the misregistration
// (empty cells collapse to nothing); pass 2 draws every cell over it, with its velocity fill,
// the playhead tint and the focus ring worked out in the fragment shader.

/** Corner radius of a cell and the width and distance of the rings, in CSS pixels. */
export const RADIUS = 4;
export const RING_GAP = 1;
export const RING_WIDTH = 2;
/** How much of the playhead colour an empty step in its column takes. */
export const PLAYHEAD_MIX = 0.3;

const VERTEX = `#version 300 es
layout(location = 0) in vec2 a_corner;
layout(location = 1) in vec4 a_rect;
layout(location = 2) in vec3 a_cell;
uniform vec2 u_size;
uniform vec2 u_offset;
uniform float u_margin;
uniform float u_shadow;
out vec2 v_local;
out vec2 v_cellSize;
flat out vec3 v_cell;
void main() {
  // The shadow pass draws lit cells only: an empty one, or a roll preview's note bar (flag 8),
  // collapses to a point.
  bool bar = (int(a_cell.z + 0.5) & 8) != 0;
  float keep = (u_shadow > 0.5 && (a_cell.y < 0.0 || bar)) ? 0.0 : 1.0;
  vec2 local = mix(vec2(-u_margin), a_rect.zw + u_margin, a_corner) * keep;
  vec2 pos = a_rect.xy + u_offset + vec2(${EDGE.toFixed(1)}) + local;
  v_local = local;
  v_cellSize = a_rect.zw;
  v_cell = a_cell;
  vec2 clip = pos / u_size * 2.0 - 1.0;
  gl_Position = vec4(clip.x, -clip.y, 0.0, 1.0);
}`;

const FRAGMENT = `#version 300 es
precision highp float;
in vec2 v_local;
in vec2 v_cellSize;
flat in vec3 v_cell;
uniform vec4 u_inks[6];
uniform vec4 u_inksB[6];
uniform vec4 u_stepA;
uniform vec4 u_stepB;
uniform vec4 u_stepLit;
uniform vec4 u_focus;
uniform vec4 u_playhead;
uniform float u_shadow;
uniform float u_dpr;
out vec4 outColor;

float roundedBox(vec2 p, vec2 halfSize, float r) {
  vec2 q = abs(p) - halfSize + r;
  return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

void main() {
  int ink = clamp(int(v_cell.x + 0.5), 0, 5);
  float velocity = v_cell.y;
  int flags = int(v_cell.z + 0.5);
  bool focus = (flags & 1) != 0;
  bool playhead = (flags & 2) != 0;
  bool beatB = (flags & 4) != 0;
  bool bar = (flags & 8) != 0;

  vec2 halfSize = v_cellSize * 0.5;
  float d = roundedBox(v_local - halfSize, halfSize, min(${RADIUS.toFixed(1)}, min(halfSize.x, halfSize.y)));
  float cover = clamp(0.5 - d * u_dpr, 0.0, 1.0);

  vec4 c;
  if (u_shadow > 0.5) {
    c = u_inksB[ink];
  } else if (bar) {
    c = u_inks[ink];
  } else if (velocity < 0.0) {
    c = beatB ? u_stepB : u_stepA;
    if (playhead) c.rgb = mix(c.rgb, u_playhead.rgb, ${PLAYHEAD_MIX.toFixed(2)});
  } else {
    float fromBottom = v_cellSize.y - v_local.y;
    c = fromBottom <= v_cellSize.y * clamp(velocity, 0.0, 1.0) ? u_inks[ink] : u_stepLit;
  }
  vec4 cell = vec4(c.rgb * c.a, c.a) * cover;

  // A ring just outside the cell: focus, else the playhead around a lit step.
  vec4 ring = vec4(0.0);
  if (u_shadow < 0.5 && !bar && (focus || (playhead && velocity >= 0.0))) {
    vec4 rc = focus ? u_focus : u_playhead;
    float mid = ${(RING_GAP + RING_WIDTH / 2).toFixed(1)};
    float band = abs(d - mid) - ${(RING_WIDTH / 2).toFixed(1)};
    float rcover = clamp(0.5 - band * u_dpr, 0.0, 1.0) * rc.a;
    ring = vec4(rc.rgb * rcover, rcover);
  }
  outColor = ring + cell * (1.0 - ring.a);
}`;

const UNIFORMS = [
  "u_size",
  "u_offset",
  "u_margin",
  "u_shadow",
  "u_inks",
  "u_inksB",
  "u_stepA",
  "u_stepB",
  "u_stepLit",
  "u_focus",
  "u_playhead",
  "u_dpr",
] as const;
type Uniform = (typeof UNIFORMS)[number];

function compile(gl: WebGL2RenderingContext, type: number, source: string): WebGLShader | null {
  const shader = gl.createShader(type);
  if (!shader) return null;
  gl.shaderSource(shader, source);
  gl.compileShader(shader);
  if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS) && !gl.isContextLost()) {
    console.error(`Step grid shader: ${gl.getShaderInfoLog(shader) ?? "did not compile"}`);
    gl.deleteShader(shader);
    return null;
  }
  return shader;
}

function link(gl: WebGL2RenderingContext): WebGLProgram | null {
  const vs = compile(gl, gl.VERTEX_SHADER, VERTEX);
  const fs = compile(gl, gl.FRAGMENT_SHADER, FRAGMENT);
  const program = vs && fs ? gl.createProgram() : null;
  if (program && vs && fs) {
    gl.attachShader(program, vs);
    gl.attachShader(program, fs);
    gl.linkProgram(program);
  }
  if (vs) gl.deleteShader(vs);
  if (fs) gl.deleteShader(fs);
  if (program && !gl.getProgramParameter(program, gl.LINK_STATUS) && !gl.isContextLost()) {
    console.error(`Step grid program: ${gl.getProgramInfoLog(program) ?? "did not link"}`);
    gl.deleteProgram(program);
    return null;
  }
  return program;
}

const flatten = (colours: number[][]) => new Float32Array(colours.flat());

/** A WebGL2 renderer for the canvas, or `null` when WebGL2 (or its shaders) is not available. */
export function createGlRenderer(canvas: HTMLCanvasElement): GridRenderer | null {
  const gl = canvas.getContext("webgl2", { antialias: false, premultipliedAlpha: true, alpha: true });
  if (!gl) return null;
  const program = link(gl);
  if (!program) return null;

  const uniforms = {} as Record<Uniform, WebGLUniformLocation | null>;
  for (const name of UNIFORMS) uniforms[name] = gl.getUniformLocation(program, name);

  const vao = gl.createVertexArray();
  const quad = gl.createBuffer();
  const cells = gl.createBuffer();
  let capacity = 0;
  gl.bindVertexArray(vao);

  // Location 0: the unit quad's corners, as a triangle strip.
  gl.bindBuffer(gl.ARRAY_BUFFER, quad);
  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([0, 0, 1, 0, 0, 1, 1, 1]), gl.STATIC_DRAW);
  gl.enableVertexAttribArray(0);
  gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);

  // Locations 1 and 2: per cell, x/y/w/h then ink/velocity/flags (STRIDE floats a record).
  const bytes = STRIDE * 4;
  gl.bindBuffer(gl.ARRAY_BUFFER, cells);
  gl.enableVertexAttribArray(1);
  gl.vertexAttribPointer(1, 4, gl.FLOAT, false, bytes, 0);
  gl.vertexAttribDivisor(1, 1);
  gl.enableVertexAttribArray(2);
  gl.vertexAttribPointer(2, 3, gl.FLOAT, false, bytes, 16);
  gl.vertexAttribDivisor(2, 1);
  gl.bindVertexArray(null);

  return {
    draw(instances: Float32Array, layout: GridLayout, palette: Palette) {
      if (gl.isContextLost()) return;
      const count = instances.length / STRIDE;
      gl.viewport(0, 0, canvas.width, canvas.height);
      gl.clearColor(0, 0, 0, 0);
      gl.clear(gl.COLOR_BUFFER_BIT);
      if (count === 0) return;

      gl.useProgram(program);
      gl.bindVertexArray(vao);
      gl.bindBuffer(gl.ARRAY_BUFFER, cells);
      if (instances.length > capacity) {
        gl.bufferData(gl.ARRAY_BUFFER, instances, gl.DYNAMIC_DRAW);
        capacity = instances.length;
      } else {
        gl.bufferSubData(gl.ARRAY_BUFFER, 0, instances);
      }

      gl.enable(gl.BLEND);
      gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA);
      gl.uniform2f(uniforms.u_size, canvas.width / layout.dpr, canvas.height / layout.dpr);
      gl.uniform1f(uniforms.u_dpr, layout.dpr);
      gl.uniform4fv(uniforms.u_inks, flatten(palette.inks));
      gl.uniform4fv(uniforms.u_inksB, flatten(palette.inksB));
      gl.uniform4fv(uniforms.u_stepA, palette.stepA);
      gl.uniform4fv(uniforms.u_stepB, palette.stepB);
      gl.uniform4fv(uniforms.u_stepLit, palette.stepLit);
      gl.uniform4fv(uniforms.u_focus, palette.focus);
      gl.uniform4fv(uniforms.u_playhead, palette.playhead);

      // Pass 1: the second ink, offset, under lit cells.
      gl.uniform1f(uniforms.u_shadow, 1);
      gl.uniform2f(uniforms.u_offset, palette.offset[0], palette.offset[1]);
      gl.uniform1f(uniforms.u_margin, 0);
      gl.drawArraysInstanced(gl.TRIANGLE_STRIP, 0, 4, count);

      // Pass 2: the cells, with room around each for its ring.
      gl.uniform1f(uniforms.u_shadow, 0);
      gl.uniform2f(uniforms.u_offset, 0, 0);
      gl.uniform1f(uniforms.u_margin, RING_GAP + RING_WIDTH + 1);
      gl.drawArraysInstanced(gl.TRIANGLE_STRIP, 0, 4, count);
      gl.bindVertexArray(null);
    },
    dispose() {
      gl.deleteBuffer(quad);
      gl.deleteBuffer(cells);
      gl.deleteVertexArray(vao);
      gl.deleteProgram(program);
    },
  };
}
