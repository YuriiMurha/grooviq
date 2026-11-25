import { FALLBACK, parseColor, type Rgba } from "../rack/palette";
import type { PlaylistView } from "./geometry";
import type { GroupPreview } from "./groupGesture";
import { splitTick, type RenderClip } from "./viewport";

export type PlaylistPalette = Readonly<{ inks: readonly Rgba[]; bg: Rgba; line: Rgba; select: Rgba }>;
export type PlaylistScene = Readonly<{
  view: PlaylistView; dpr: number; barTicks: number; selected: number;
  /** A native clip index is transformed in the shader; -1 draws a local copy/place preview. */
  preview: { index: number; clip: RenderClip } | null;
  group?: GroupPreview | null;
}>;
export interface PlaylistRenderer {
  readonly kind: "webgl2" | "canvas2d";
  setClips(clips: readonly RenderClip[]): void;
  setGroupSelection(ids: ReadonlySet<number>): void;
  draw(scene: PlaylistScene, palette: PlaylistPalette): void;
  dispose(): void;
}
export function readPlaylistPalette(root: Element): PlaylistPalette {
  const style = getComputedStyle(root);
  const color = (token: string): Rgba => parseColor(style.getPropertyValue(token)) ?? [...FALLBACK];
  return { inks: [1, 2, 3, 4, 5, 6].map(n => color(`--ink-${n}`)), bg: color("--bg"), line: color("--line"), select: color("--select") };
}
const VERTEX = `#version 300 es
layout(location=0) in vec2 corner;
layout(location=1) in vec4 rect;
layout(location=2) in vec4 style;
layout(location=3) in float groupMember;
uniform vec2 size;
uniform vec2 camera;
uniform float scrollRow;
uniform float tickWidth;
uniform float rowHeight;
uniform int selected;
uniform int dragIndex;
uniform vec4 drag;
uniform int groupPass;
uniform vec3 groupOffset;
out vec2 local;
out vec2 extent;
flat out vec4 flags;
void main(){
  vec4 r=gl_InstanceID==dragIndex?drag:rect;
  bool member=groupMember>0.5;
  if(member && groupPass!=0) r.xyz+=groupOffset;
  float x=((r.x-camera.x)*4096.0+r.y-camera.y)*tickWidth;
  vec2 pixels=vec2(max(1.0,r.w*tickWidth),min(42.0,rowHeight-22.0));
  vec2 position=vec2(x,(r.z-scrollRow)*rowHeight+18.0)+corner*pixels;
  gl_Position=vec4(position/size*vec2(2.0,-2.0)+vec2(-1.0,1.0),0.0,1.0);
  if(groupPass==2 && !member) gl_Position=vec4(2.0,2.0,0.0,1.0);
  local=corner*pixels;extent=pixels;
  flags=vec4(style.x,float(gl_InstanceID==selected || member),style.z,style.w);
}`;
const FRAGMENT = `#version 300 es
precision highp float;
in vec2 local;
in vec2 extent;
flat in vec4 flags;
uniform vec4 inks[6];
uniform vec4 selection;
uniform float dpr;
out vec4 color;
void main(){
  vec4 ink=inks[clamp(int(flags.x+0.5),0,5)];
  float edge=min(min(local.x,extent.x-local.x),min(local.y,extent.y-local.y));
  bool outline=edge<(flags.y>0.5?2.0:1.0);
  float opacity=flags.z>0.5?0.28:1.0;
  vec4 chosen=outline?(flags.y>0.5?selection:ink):vec4(ink.rgb,0.22);
  // Existing audio/automation records remain visible, without invented editable previews.
  if(flags.w>0.5 && !outline)chosen.a=0.12;
  float alpha=chosen.a*opacity*clamp(edge*dpr,0.0,1.0);
  color=vec4(chosen.rgb*alpha,alpha);
}`;
const GRID_VERTEX = `#version 300 es
void main(){
  vec2 p=vec2(float((gl_VertexID<<1)&2),float(gl_VertexID&2));
  gl_Position=vec4(p*2.0-1.0,0.0,1.0);
}`;
const GRID_FRAGMENT = `#version 300 es
precision highp float;
uniform vec2 size;
uniform vec2 phase;
uniform float barPixels;
uniform float rowPixels;
uniform float dpr;
uniform vec4 ground;
uniform vec4 line;
out vec4 color;
void main(){
  vec2 p=vec2(gl_FragCoord.x/dpr,size.y-gl_FragCoord.y/dpr)+phase;
  float bar=mod(p.x,barPixels);
  float quarter=mod(p.x,barPixels/4.0);
  float row=mod(p.y,rowPixels);
  float strength=(row<1.0||bar<1.0)?0.9:(quarter<1.0?0.35:0.0);
  color=mix(ground,line,strength);
}`;
function shader(gl: WebGL2RenderingContext, kind: number, text: string): WebGLShader | null {
  const shader = gl.createShader(kind); if (!shader) return null;
  gl.shaderSource(shader, text); gl.compileShader(shader);
  if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) { gl.deleteShader(shader); return null; }
  return shader;
}
function programFor(gl: WebGL2RenderingContext, vertex: string, fragment: string): WebGLProgram | null {
  const vs = shader(gl, gl.VERTEX_SHADER, vertex), fs = shader(gl, gl.FRAGMENT_SHADER, fragment);
  const program = vs && fs ? gl.createProgram() : null;
  if (program && vs && fs) { gl.attachShader(program, vs); gl.attachShader(program, fs); gl.linkProgram(program); }
  if (vs) gl.deleteShader(vs); if (fs) gl.deleteShader(fs);
  if (!program || !gl.getProgramParameter(program, gl.LINK_STATUS)) { if (program) gl.deleteProgram(program); return null; }
  return program;
}
function gpu(canvas: HTMLCanvasElement): PlaylistRenderer | null {
  const gl = canvas.getContext("webgl2", { antialias: false, alpha: false }); if (!gl) return null;
  const program = programFor(gl, VERTEX, FRAGMENT), grid = programFor(gl, GRID_VERTEX, GRID_FRAGMENT);
  if (!program || !grid) { if (program) gl.deleteProgram(program); if (grid) gl.deleteProgram(grid); return null; }
  const vao = gl.createVertexArray(), vertices = gl.createBuffer(), instances = gl.createBuffer(), previewBuffer = gl.createBuffer(), membership = gl.createBuffer();
  if (!vao || !vertices || !instances || !previewBuffer || !membership) {
    if (vao) gl.deleteVertexArray(vao); for (const b of [vertices, instances, previewBuffer, membership]) if (b) gl.deleteBuffer(b); gl.deleteProgram(program); gl.deleteProgram(grid); return null;
  }
  const uniforms = Object.fromEntries(["size", "camera", "scrollRow", "tickWidth", "rowHeight", "selected", "dragIndex", "drag", "groupPass", "groupOffset", "inks[0]", "selection", "dpr"].map(name => [name, gl.getUniformLocation(program, name)]));
  const gridUniforms = Object.fromEntries(["size", "phase", "barPixels", "rowPixels", "dpr", "ground", "line"].map(name => [name, gl.getUniformLocation(grid, name)]));
  gl.bindVertexArray(vao); gl.bindBuffer(gl.ARRAY_BUFFER, vertices);
  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([0, 0, 1, 0, 0, 1, 0, 1, 1, 0, 1, 1]), gl.STATIC_DRAW);
  gl.enableVertexAttribArray(0); gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);
  const bindInstances = (buffer: WebGLBuffer) => {
    gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
    for (const [attribute, offset] of [[1, 0], [2, 16]]) {
      gl.enableVertexAttribArray(attribute); gl.vertexAttribPointer(attribute, 4, gl.FLOAT, false, 32, offset); gl.vertexAttribDivisor(attribute, 1);
    }
  };
  const packed = (clip: RenderClip): number[] => [...splitTick(clip.startTick), clip.trackIndex, clip.lengthTicks, clip.ink, 0, Number(clip.muted), Number(clip.kind !== "pattern")];
  let clips: readonly RenderClip[] = [];
  let groupIds: ReadonlySet<number> = new Set();
  const uploadMembership = () => {
    const flags = new Uint8Array(clips.length);
    if (groupIds.size) clips.forEach((clip, index) => { if (clip.id !== null && groupIds.has(clip.id)) flags[index] = 1; });
    gl.bindBuffer(gl.ARRAY_BUFFER, membership);
    gl.bufferData(gl.ARRAY_BUFFER, flags, gl.DYNAMIC_DRAW);
  };
  return {
    kind: "webgl2",
    setClips(next) {
      clips = next;
      const data = new Float32Array(clips.length * 8);
      clips.forEach((clip, i) => data.set(packed(clip), i * 8));
      gl.bindBuffer(gl.ARRAY_BUFFER, instances); gl.bufferData(gl.ARRAY_BUFFER, data, gl.STATIC_DRAW);
      uploadMembership();
    },
    setGroupSelection(ids) { groupIds = ids; uploadMembership(); },
    draw(scene, palette) {
      if (gl.isContextLost()) return;
      const { view, preview } = scene; gl.viewport(0, 0, canvas.width, canvas.height);
      gl.clearColor(...palette.bg); gl.clear(gl.COLOR_BUFFER_BIT); gl.disable(gl.BLEND);
      gl.useProgram(grid); gl.bindVertexArray(vao);
      gl.uniform2f(gridUniforms.size, view.width, view.height); gl.uniform2f(gridUniforms.phase, (view.scrollTick % scene.barTicks) * view.tickWidth, (view.scrollTrack % 1) * view.rowHeight);
      gl.uniform1f(gridUniforms.barPixels, scene.barTicks * view.tickWidth); gl.uniform1f(gridUniforms.rowPixels, view.rowHeight); gl.uniform1f(gridUniforms.dpr, scene.dpr);
      gl.uniform4f(gridUniforms.ground, ...palette.bg); gl.uniform4f(gridUniforms.line, ...palette.line); gl.drawArrays(gl.TRIANGLES, 0, 3);
      gl.enable(gl.BLEND); gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA);
      gl.useProgram(program); gl.bindVertexArray(vao);
      gl.uniform2f(uniforms.size, view.width, view.height); gl.uniform2f(uniforms.camera, ...splitTick(view.scrollTick));
      gl.uniform1f(uniforms.scrollRow, view.scrollTrack); gl.uniform1f(uniforms.tickWidth, view.tickWidth); gl.uniform1f(uniforms.rowHeight, view.rowHeight);
      gl.uniform1i(uniforms.selected, scene.selected); gl.uniform1i(uniforms.dragIndex, preview?.index ?? -1);
      if (preview) gl.uniform4f(uniforms.drag, ...splitTick(preview.clip.startTick), preview.clip.trackIndex, preview.clip.lengthTicks);
      gl.uniform4fv(uniforms["inks[0]"], new Float32Array(palette.inks.flat())); gl.uniform4f(uniforms.selection, ...palette.select); gl.uniform1f(uniforms.dpr, scene.dpr);
      const group = scene.group;
      if (group) {
        // The high part can be negative. Both parts stay exact near the camera.
        const high = Math.floor(group.deltaTicks / 4096);
        gl.uniform3f(uniforms.groupOffset, high, group.deltaTicks - high * 4096, group.deltaTracks);
      }
      gl.uniform1i(uniforms.groupPass, group?.kind === "move" ? 1 : 0);
      bindInstances(instances);
      gl.bindBuffer(gl.ARRAY_BUFFER, membership);
      gl.enableVertexAttribArray(3); gl.vertexAttribPointer(3, 1, gl.UNSIGNED_BYTE, false, 0, 0); gl.vertexAttribDivisor(3, 1);
      gl.drawArraysInstanced(gl.TRIANGLES, 0, 6, clips.length);
      if (group?.kind === "copy") {
        gl.uniform1i(uniforms.groupPass, 2);
        gl.drawArraysInstanced(gl.TRIANGLES, 0, 6, clips.length);
      }
      if (preview?.index === -1) {
        gl.uniform1i(uniforms.groupPass, 0);
        gl.disableVertexAttribArray(3); gl.vertexAttrib1f(3, 0);
        gl.uniform1i(uniforms.dragIndex, -1); gl.uniform1i(uniforms.selected, 0);
        gl.bindBuffer(gl.ARRAY_BUFFER, previewBuffer); gl.bufferData(gl.ARRAY_BUFFER, new Float32Array(packed(preview.clip)), gl.DYNAMIC_DRAW);
        bindInstances(previewBuffer); gl.drawArraysInstanced(gl.TRIANGLES, 0, 6, 1);
      }
    },
    dispose() { for (const b of [vertices, instances, previewBuffer, membership]) gl.deleteBuffer(b); gl.deleteVertexArray(vao); gl.deleteProgram(program); gl.deleteProgram(grid); gl.getExtension("WEBGL_lose_context")?.loseContext(); },
  };
}
function fallback(canvas: HTMLCanvasElement): PlaylistRenderer | null {
  const ctx = canvas.getContext("2d"); if (!ctx) return null;
  let clips: readonly RenderClip[] = [];
  let groupIds: ReadonlySet<number> = new Set();
  const css = (c: Rgba, alpha = c[3]) => `rgba(${c.slice(0, 3).map(n => Math.round(n * 255)).join(",")},${alpha})`;
  return { kind: "canvas2d", setClips: next => { clips = next; }, setGroupSelection: ids => { groupIds = ids; }, dispose() {}, draw(scene, palette) {
    const v = scene.view; ctx.setTransform(scene.dpr, 0, 0, scene.dpr, 0, 0); ctx.fillStyle = css(palette.bg); ctx.fillRect(0, 0, v.width, v.height);
    ctx.strokeStyle = css(palette.line); ctx.lineWidth = 1;
    const step = scene.barTicks * v.tickWidth / 4;
    for (let x = -(v.scrollTick * v.tickWidth) % step; x < v.width; x += step) { ctx.beginPath(); ctx.moveTo(x + 0.5, 0); ctx.lineTo(x + 0.5, v.height); ctx.stroke(); }
    for (let y = -(v.scrollTrack * v.rowHeight) % v.rowHeight; y < v.height; y += v.rowHeight) { ctx.beginPath(); ctx.moveTo(0, y + 0.5); ctx.lineTo(v.width, y + 0.5); ctx.stroke(); }
    const draw = (clip: RenderClip, selected: boolean, group?: GroupPreview) => {
      const x = (clip.startTick + (group?.deltaTicks ?? 0) - v.scrollTick) * v.tickWidth;
      const y = (clip.trackIndex + (group?.deltaTracks ?? 0) - v.scrollTrack) * v.rowHeight + 18;
      const width = Math.max(1, clip.lengthTicks * v.tickWidth);
      if (x + width <= 0 || x >= v.width || y + 42 <= 0 || y >= v.height) return;
      ctx.fillStyle = css(palette.inks[clip.ink] ?? [...FALLBACK], clip.muted ? 0.08 : 0.22); ctx.fillRect(x, y, width, 42);
      ctx.strokeStyle = css(selected ? palette.select : palette.inks[clip.ink] ?? [...FALLBACK]); ctx.lineWidth = selected ? 2 : 1; ctx.strokeRect(x + 0.5, y + 0.5, width - 1, 41);
    };
    const member = (clip: RenderClip) => clip.id !== null && groupIds.has(clip.id);
    const group = scene.group;
    clips.forEach((clip, index) => draw(scene.preview?.index === index ? scene.preview.clip : clip,
      index === scene.selected || member(clip), group?.kind === "move" && member(clip) ? group : undefined));
    if (group?.kind === "copy") for (const clip of clips) if (member(clip)) draw(clip, true, group);
    if (scene.preview?.index === -1) draw(scene.preview.clip, true);
  } };
}
/** Try a detached canvas first: a failed GL context must not consume the 2D fallback canvas. */
export function createPlaylistRenderer(canvas: HTMLCanvasElement): PlaylistRenderer | null {
  const probe = document.createElement("canvas").getContext("webgl2");
  if (probe) { probe.getExtension("WEBGL_lose_context")?.loseContext(); return gpu(canvas); }
  return fallback(canvas);
}
