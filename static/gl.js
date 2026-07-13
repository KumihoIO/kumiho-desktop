// Kumiho Brain — WebGL2 render core.
//
// Architecture (kumiho-SDKs#57 M2): particle motion runs GPU-side via
// transform feedback (spring-to-anchor + curl-ish drift integrated in a TF
// pass); points render as instanced billboard quads reading the TF buffer
// directly; edge lines fetch endpoint positions from an RGBA32F position
// texture refreshed each frame by a GPU→GPU PIXEL_UNPACK copy of the TF
// buffer. If that pipeline misbehaves on a driver, a runtime health check
// flips to a stateless-drift fallback (same look, motion recomputed from the
// anchor texture in each shader). No per-frame CPU work per point either way.

'use strict';

const POS_TEX_W = 1024; // position texture width; index = (i % W, i >> 10)

// ---------------------------------------------------------------- shaders

const SIM_VS = `#version 300 es
precision highp float;
in vec4 aPos;
in vec4 aVel;
in vec4 aAnchor;   // xyz anchor, w phase
uniform float uDt, uTime;
out vec4 vPos;
out vec4 vVel;
vec3 swirl(vec3 p, float ph){
  float t = uTime*0.22 + ph;
  return vec3(sin(t*1.10 + p.y*2.7), cos(t*0.87 + p.z*2.3), sin(t*1.31 + p.x*2.1));
}
void main(){
  vec3 pos = aPos.xyz, vel = aVel.xyz;
  vec3 force = 3.4*(aAnchor.xyz - pos) + 0.10*swirl(pos*1.6, aAnchor.w);
  vel = (vel + force*uDt) * exp(-2.8*uDt);
  pos += vel*uDt;
  vPos = vec4(pos, 1.0);
  vVel = vec4(vel, 0.0);
}`;

const SIM_FS = `#version 300 es
precision mediump float;
void main(){}`;

// Stateless drift for the fallback path — same character as the sim's swirl.
const DRIFT = `
vec3 drift(vec3 a, float ph, float t){
  float s = t*0.22 + ph;
  return a + 0.030*vec3(sin(s*1.10 + a.y*2.7), cos(s*0.87 + a.z*2.3), sin(s*1.31 + a.x*2.1));
}`;

const POINT_VS = `#version 300 es
precision highp float;
in vec2 aCorner;
in vec4 iPos;      // live position (TF output)
in vec4 iAnchor;   // xyz anchor, w phase
in vec4 iMeta;     // x birth, y hue (0 conv / 1 code), z baseSize, w seed01
in vec2 iState;    // x flag (1 match, 0 dim, -1 dead), y space id
uniform mat4 uProj, uView;
uniform vec2 uViewport;
uniform float uTime, uSearch, uSel, uSpaceHi, uStateless, uPixelScale;
out vec2 vC;
out vec3 vCol;
out float vGlow, vBirth, vSel, vCore;
${DRIFT}
vec3 ambient(float t){
  float a = t*0.10;
  return vec3(1.0) + 0.10*vec3(sin(a), sin(a+2.1), sin(a+4.2));
}
void main(){
  vC = vec2(0.0); vCol = vec3(0.0); vGlow = 0.0; vBirth = 9.0; vSel = 0.0; vCore = 0.0;
  if (iState.x < -0.5) { gl_Position = vec4(2.0, 2.0, 2.0, 1.0); return; }
  vec3 world = (uStateless > 0.5) ? drift(iAnchor.xyz, iAnchor.w, uTime) : iPos.xyz;
  float age = uTime - iMeta.x;
  float pop = (iMeta.x > 0.0 && age < 3.0) ? exp(-age*1.7)*2.4 : 0.0;
  vec4 view = uView * vec4(world, 1.0);
  float dist = max(0.6, -view.z);
  float pulse = 0.72 + 0.28*sin(uTime*0.5 + iAnchor.w*3.0);
  float sel = (abs(float(gl_InstanceID) - uSel) < 0.5) ? 1.0 : 0.0;
  float on = (uSearch > 0.5) ? (iState.x > 0.5 ? 1.0 : 0.06) : 1.0;
  if (uSpaceHi > -0.5) on *= (abs(iState.y - uSpaceHi) < 0.5) ? 1.0 : 0.10;
  float fog = clamp((7.2 - dist) / 3.0, 0.38, 1.0);
  vGlow = (0.9 + 0.55*pulse + pop + sel*1.3) * fog * on;
  vec3 conv = vec3(0.50, 0.82, 1.00);
  vec3 code = vec3(1.00, 0.60, 0.26);
  vCol = mix(conv, code, iMeta.y) * ambient(uTime);
  vCol = mix(vCol, vec3(1.0), sel*0.25 + (uSearch > 0.5 ? iState.x*0.15 : 0.0));
  vBirth = (iMeta.x > 0.0) ? age : 9.0;
  vSel = sel;
  vCore = 0.55 + 0.45*iMeta.w;
  float px = iMeta.z * (0.95 + 0.40*pulse + pop*2.0 + sel*1.6) * uPixelScale * 3.9 / dist;
  px = clamp(px, 1.5, 220.0);
  vec4 clip = uProj * view;
  clip.xy += aCorner * (px * 2.0 / uViewport) * clip.w;
  gl_Position = clip;
  vC = aCorner;
}`;

const POINT_FS = `#version 300 es
precision mediump float;
in vec2 vC;
in vec3 vCol;
in float vGlow, vBirth, vSel, vCore;
out vec4 frag;
void main(){
  float d = dot(vC, vC);
  if (d > 1.0) discard;
  float core = pow(max(0.0, 1.0 - d), 6.0) * (1.1 + vCore);
  float halo = pow(max(0.0, 1.0 - d), 2.2) * 0.30;
  float r = sqrt(d);
  float ring = 0.0;
  if (vBirth < 1.6) {
    float rt = clamp(vBirth * 0.7, 0.0, 1.0);
    ring = exp(-pow((r - rt) * 12.0, 2.0)) * (1.0 - rt) * 1.6;
  }
  float selRing = vSel * exp(-pow((r - 0.82) * 16.0, 2.0)) * 0.8;
  vec3 c = vCol * (core + halo + ring + selRing) * vGlow;
  frag = vec4(c, 1.0);
}`;

const EDGE_VS = `#version 300 es
precision highp float;
in float aIdx;     // this endpoint's node index
in float aOther;   // other endpoint's node index
in float aT;       // 0 at src, 1 at dst
in float aType;    // edge-type palette index
in vec2 aSpaces;   // space ids of (src, dst)
uniform sampler2D uPosTex;
uniform sampler2D uAnchorTex;
uniform sampler2D uMatchTex;   // per-node search/filter match flag (0 or 1)
uniform mat4 uProj, uView;
uniform float uTime, uSearch, uSel, uSpaceHi, uStateless;
uniform float uAmbient;       // 1 = proximity filament layer (render texture,
                              // not data): fainter, colorless, no audit affordances
uniform vec4 uPulse;          // x node id, y start time
uniform vec3 uTypeCol[8];
out vec3 vCol;
out float vA, vT, vPulse;
${DRIFT}
vec3 nodePos(float idx){
  int i = int(idx + 0.5);
  ivec2 uv = ivec2(i & ${POS_TEX_W - 1}, i >> 10);
  if (uStateless > 0.5) {
    vec4 aw = texelFetch(uAnchorTex, uv, 0);
    return drift(aw.xyz, aw.w, uTime);
  }
  return texelFetch(uPosTex, uv, 0).xyz;
}
float nodeMatch(float idx){
  int i = int(idx + 0.5);
  return texelFetch(uMatchTex, ivec2(i & ${POS_TEX_W - 1}, i >> 10), 0).r;
}
void main(){
  vec3 world = nodePos(aIdx);
  vec3 other = nodePos(aOther);
  vec4 view = uView * vec4(world, 1.0);
  gl_Position = uProj * view;
  float dist = max(0.6, -view.z);
  float fog = clamp((7.2 - dist) / 3.2, 0.14, 1.0);
  // short local links form the web; the rare long haul fades back
  float len = distance(world, other);
  float lfog = mix(0.28, 1.0, smoothstep(1.5, 0.45, len));
  float shim = 0.55 + 0.45*sin(uTime*0.7 + (aIdx + aOther)*0.37);
  float hi = (1.0 - uAmbient) * ((abs(aIdx - uSel) < 0.5 || abs(aOther - uSel) < 0.5) ? 1.0 : 0.0);
  // during a filter, keep edges whose BOTH endpoints match (the interlinks
  // inside the selection) lit; fade the rest right back
  float both = step(0.5, nodeMatch(aIdx)) * step(0.5, nodeMatch(aOther));
  float on = (uSearch > 0.5) ? (both > 0.5 ? mix(1.0, 0.55, uAmbient) : 0.05) : 1.0;
  if (uSpaceHi > -0.5) on *= (abs(aSpaces.x - uSpaceHi) < 0.5 && abs(aSpaces.y - uSpaceHi) < 0.5) ? 1.0 : 0.10;
  vCol = mix(uTypeCol[int(aType + 0.5)], vec3(0.74, 0.82, 0.98), uAmbient);
  float base = mix(0.16 + 0.11*shim, 0.105 + 0.07*shim, uAmbient);
  vA = base * fog * lfog * on + hi * 0.6;
  vT = aT;
  float touched = (1.0 - uAmbient) * ((abs(aIdx - uPulse.x) < 0.5 || abs(aOther - uPulse.x) < 0.5) ? 1.0 : 0.0);
  vPulse = touched * max(0.0, 1.0 - (uTime - uPulse.y) * 0.5);
}`;

const EDGE_FS = `#version 300 es
precision highp float; // matches the VS: uTime/uPulse are shared across stages
in vec3 vCol;
in float vA, vT, vPulse;
uniform float uTime;
uniform vec4 uPulse;
out vec4 frag;
void main(){
  float a = vA;
  if (vPulse > 0.0) {
    float p = fract((uTime - uPulse.y) * 0.9);
    a += exp(-pow((vT - p) * 8.0, 2.0)) * vPulse * 1.4;
  }
  frag = vec4(vCol * a, 1.0);
}`;

const QUAD_VS = `#version 300 es
precision highp float;
in vec2 aP;
out vec2 vC;
void main(){ vC = aP; gl_Position = vec4(aP, 0.0, 1.0); }`;

const NEBULA_FS = `#version 300 es
precision highp float;
in vec2 vC;
out vec4 frag;
uniform vec2 uCenter;
uniform float uAspect, uTime, uInt, uRad, uSpaces;
uniform sampler2D uFieldTex;   // JWST Pillars image, shown in spaces mode
uniform float uFieldReady;     // 1 once the image texture has loaded
uniform sampler2D uOrbTex;     // JWST Southern Ring image, shown in unified mode
uniform float uOrbReady;
uniform float uFieldAspect, uOrbAspect;  // image w/h, for aspect-preserving cover fit
float h21(vec2 p){ return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453); }
float vn(vec2 p){
  vec2 i = floor(p), f = fract(p);
  vec2 u = f*f*(3.0-2.0*f);
  return mix(mix(h21(i), h21(i+vec2(1,0)), u.x), mix(h21(i+vec2(0,1)), h21(i+vec2(1,1)), u.x), u.y);
}
float fbm(vec2 p){
  float v = 0.0, a = 0.5;
  for (int k = 0; k < 5; k++){ v += a*vn(p); p = p*2.03 + vec2(17.3, 9.1); a *= 0.5; }
  return v;
}
// aspect-preserving 'cover' fit: fills the viewport, crops overflow, no stretch
vec2 cover(vec2 uv, float ia){
  float sc = max(uAspect / ia, 1.0);
  return (uv - 0.5) * (vec2(uAspect, 1.0) / (vec2(ia, 1.0) * sc)) + 0.5;
}
void main(){
  vec2 d = vC - uCenter;
  d.x *= uAspect;
  float r = length(d);
  // spaces deep-field ('Pillars of Creation' feel): sculptural rust-dust
  // columns with warm star-formation rims over a blue-teal star field — the
  // synapse<->cosmos look; blended in by uSpaces for a smooth crossfade
  vec3 col2;
  {
    vec2 pp = vC * vec2(uAspect, 1.0);
    // vertically-streaked density -> finger-like columns (low X-freq stretch)
    float w = fbm(vec2(pp.x * 1.3, pp.y * 0.5) + vec2(0.0, uTime * 0.010));
    float ds = fbm(vec2(pp.x * 2.0 + w * 0.7, pp.y * 0.75 + w * 1.3 + uTime * 0.004));
    // pillars rise from the bottom into the mid-frame, concentrated centrally
    float env = smoothstep(0.45, -1.0, vC.y) * smoothstep(1.5, 0.15, abs(pp.x));
    float pillar = smoothstep(0.44, 0.70, ds) * env;   // rust column bodies
    float rim = smoothstep(0.34, 0.44, ds) * (1.0 - smoothstep(0.54, 0.74, ds)) * env;
    col2 = vec3(0.02, 0.05, 0.12)                       // deep blue field
         + vec3(0.34, 0.22, 0.13) * pillar * 1.30        // rust columns (visible)
         + vec3(1.00, 0.60, 0.30) * rim * 0.95           // warm star-forming rim
         + vec3(0.07, 0.17, 0.36) * (1.0 - pillar) * fbm(pp * 1.0 + 4.0) * 0.18; // faint haze
    vec2 gp = pp * 9.0, gi = floor(gp), gf = fract(gp) - 0.5;
    float sb = max(0.0, h21(gi) - 0.86) / 0.14;
    float star = smoothstep(0.05 * sb + 0.004, 0.0, length(gf)) * sb;
    star += smoothstep(0.45, 0.0, abs(gf.x)) * smoothstep(0.014, 0.0, abs(gf.y)) * sb * 0.55;
    star += smoothstep(0.45, 0.0, abs(gf.y)) * smoothstep(0.014, 0.0, abs(gf.x)) * sb * 0.55;
    col2 += vec3(0.80, 0.88, 1.0) * star * (1.0 - pillar * 0.85); // stars dimmed in the columns
    // real JWST 'Pillars of Creation' image (NASA/ESA/CSA/STScI, public domain)
    // when it has loaded; the procedural pillars above are the graceful fallback
    vec3 img = texture(uFieldTex, cover(vec2(vC.x * 0.5 + 0.5, 0.5 - vC.y * 0.5), uFieldAspect)).rgb;
    col2 = mix(col2, img * 0.70, uFieldReady); // dim so the graph reads on top
  }
  // north-star gas: one domain warp over fbm filaments, slow churn — gentler
  // contrast so it reads as luminous gas, not muddy noise
  vec2 np = d * 2.2;
  float warp = fbm(np*1.2 + uTime*0.015);
  float n = fbm(np + warp*1.1 + vec2(0.0, uTime*0.010));
  float env = smoothstep(uRad, 0.06, r);
  float dens = env * (0.52 + 0.48*n);
  // two-temperature body: warm halo outside, a wide cool-blue core
  float inner = smoothstep(0.72, 0.0, r);
  vec3 outer = vec3(0.92, 0.46, 0.20);
  vec3 core  = vec3(0.34, 0.64, 1.00);
  vec3 col = mix(outer, core, inner) * dens;
  // soft blue-white bloom at the heart — a gentle rise, never a blown dot
  col += vec3(0.50, 0.70, 1.00) * smoothstep(0.20, 0.0, r) * 0.40;
  // faint scope sight-lines — keep the terminal signature, but subtle
  float sp = (smoothstep(0.0022, 0.0, abs(d.x)) + smoothstep(0.0022, 0.0, abs(d.y)))
           * smoothstep(0.24, 0.0, r);
  col += vec3(0.70, 0.82, 1.0) * sp * 0.22;
  // clear the lower band so the perspective floor grid reads
  col *= mix(1.0, 0.5, smoothstep(-0.45, -1.0, vC.y));
  // unified orb gets the real Southern Ring nebula once it loads (dimmed more —
  // the graph orb is dense in the centre); the procedural glow is the fallback
  vec3 orb = texture(uOrbTex, cover(vec2(vC.x * 0.5 + 0.5, 0.5 - vC.y * 0.5), uOrbAspect)).rgb;
  col = mix(col, orb * 0.55, uOrbReady);
  // crossfade unified radial glow <-> spaces deep-field
  frag = vec4(mix(col, col2, clamp(uSpaces, 0.0, 1.0)) * uInt, 1.0);
}`;

const PICK_VS = `#version 300 es
precision highp float;
in vec2 aCorner;
in vec4 iPos;
in vec4 iAnchor;
in vec4 iMeta;
in vec2 iState;
uniform mat4 uProj, uView;
uniform vec2 uViewport;
uniform float uTime, uSearch, uStateless, uPixelScale;
out vec2 vC;
flat out int vId;
${DRIFT}
void main(){
  vC = vec2(0.0); vId = 0;
  bool off = iState.x < -0.5 || (uSearch > 0.5 && iState.x < 0.5);
  if (off) { gl_Position = vec4(2.0, 2.0, 2.0, 1.0); return; }
  vec3 world = (uStateless > 0.5) ? drift(iAnchor.xyz, iAnchor.w, uTime) : iPos.xyz;
  vec4 view = uView * vec4(world, 1.0);
  float dist = max(0.6, -view.z);
  float px = clamp(iMeta.z * 1.35 * uPixelScale * 3.9 / dist, 3.0, 60.0);
  vec4 clip = uProj * view;
  clip.xy += aCorner * (px * 2.0 / uViewport) * clip.w;
  gl_Position = clip;
  vC = aCorner;
  vId = gl_InstanceID;
}`;

const PICK_FS = `#version 300 es
precision mediump float;
in vec2 vC;
flat in int vId;
out vec4 frag;
void main(){
  if (dot(vC, vC) > 1.0) discard;
  frag = vec4(float(vId & 255)/255.0, float((vId >> 8) & 255)/255.0, float((vId >> 16) & 255)/255.0, 1.0);
}`;

// ---------------------------------------------------------------- helpers

function compile(gl, type, src, tag) {
  const s = gl.createShader(type);
  gl.shaderSource(s, src);
  gl.compileShader(s);
  if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) {
    throw new Error(`[brain] ${tag} shader: ${gl.getShaderInfoLog(s)}`);
  }
  return s;
}

function program(gl, vs, fs, tag, tfVaryings) {
  const p = gl.createProgram();
  gl.attachShader(p, compile(gl, gl.VERTEX_SHADER, vs, tag + '.vs'));
  gl.attachShader(p, compile(gl, gl.FRAGMENT_SHADER, fs, tag + '.fs'));
  if (tfVaryings) gl.transformFeedbackVaryings(p, tfVaryings, gl.SEPARATE_ATTRIBS);
  gl.linkProgram(p);
  if (!gl.getProgramParameter(p, gl.LINK_STATUS)) {
    throw new Error(`[brain] ${tag} link: ${gl.getProgramInfoLog(p)}`);
  }
  return p;
}

function uniformMap(gl, prog) {
  const out = {};
  const n = gl.getProgramParameter(prog, gl.ACTIVE_UNIFORMS);
  for (let i = 0; i < n; i++) {
    const info = gl.getActiveUniform(prog, i);
    out[info.name.replace(/\[0\]$/, '')] = gl.getUniformLocation(prog, info.name);
  }
  return out;
}

const M4 = {
  ident: () => new Float32Array([1,0,0,0, 0,1,0,0, 0,0,1,0, 0,0,0,1]),
  persp(fovDeg, aspect, n, f) {
    const t = 1 / Math.tan((fovDeg * Math.PI) / 360);
    return new Float32Array([t/aspect,0,0,0, 0,t,0,0, 0,0,(f+n)/(n-f),-1, 0,0,2*f*n/(n-f),0]);
  },
  mul(a, b) {
    const o = new Float32Array(16);
    for (let c = 0; c < 4; c++) for (let r = 0; r < 4; r++) {
      o[c*4+r] = a[r]*b[c*4] + a[4+r]*b[c*4+1] + a[8+r]*b[c*4+2] + a[12+r]*b[c*4+3];
    }
    return o;
  },
  translate(x, y, z) { const m = M4.ident(); m[12]=x; m[13]=y; m[14]=z; return m; },
  rotY(r) { const c=Math.cos(r), s=Math.sin(r); return new Float32Array([c,0,-s,0, 0,1,0,0, s,0,c,0, 0,0,0,1]); },
  rotX(r) { const c=Math.cos(r), s=Math.sin(r); return new Float32Array([1,0,0,0, 0,c,s,0, 0,-s,c,0, 0,0,0,1]); },
  apply(m, x, y, z) {
    return [m[0]*x+m[4]*y+m[8]*z+m[12], m[1]*x+m[5]*y+m[9]*z+m[13],
            m[2]*x+m[6]*y+m[10]*z+m[14], m[3]*x+m[7]*y+m[11]*z+m[15]];
  },
};

function seededRand(seed) {
  let s = seed >>> 0;
  return () => {
    s = (s * 1664525 + 1013904223) >>> 0;
    return s / 4294967296;
  };
}

// Edge-type palette; order must match EDGE_TYPES in brain.js.
const TYPE_COLORS = new Float32Array([
  0.52, 0.58, 0.70,   // 0 REFERENCED      steel
  0.62, 0.48, 1.00,   // 1 DERIVED_FROM    violet
  1.00, 0.36, 0.30,   // 2 SUPERSEDES      ember
  0.20, 0.78, 0.62,   // 3 DEPENDS_ON      teal
  0.92, 0.78, 0.34,   // 4 ABOUT           gold
  0.32, 0.62, 1.00,   // 5 IMPLEMENTED_IN  blue
  1.00, 0.55, 0.85,   // 6 MOTIVATED_BY    rose
  0.42, 0.46, 0.55,   // 7 other           gray
]);

// ---------------------------------------------------------------- renderer

export class BrainGL {
  constructor(canvas) {
    this.canvas = canvas;
    this.ok = false;
    const gl = canvas.getContext('webgl2', { alpha: false, antialias: false, powerPreference: 'high-performance' });
    if (!gl) return;
    this.gl = gl;
    this.ok = true;

    this.reduce = matchMedia('(prefers-reduced-motion: reduce)').matches;
    this.stateless = this.reduce ? 1 : 0;
    this.mode = 'unified';
    this._spacesMix = 0; // 0=unified .. 1=spaces, eased for a smooth backdrop crossfade
    this._fieldReady = 0; // 1 once the Pillars image texture has loaded
    this._orbReady = 0;   // 1 once the Southern Ring image texture has loaded
    this._fieldAspect = 1.5;   // overwritten with the real w/h on image load
    this._orbAspect = 1.07;
    this.sel = -1;
    this.searchActive = 0;
    this.spaceHi = -1;
    this.pulseNode = -1;
    this.pulseT = -99;
    this.time = 0;
    this.dirty = true;
    this.fps = 0;
    this._fpsAcc = 0;
    this._fpsN = 0;
    this.verified = false;
    this.onfps = null;
    this.onclickNode = null;

    this.cam = { yaw: 0.6, pitch: 0.24, dist: 5.2, vyaw: 0, vpitch: 0, auto: 0.02,
                 tyaw: null, tpitch: null, tdist: null, lastInput: -10 };

    this.count = 0;
    this.cap = 0;
    this.nodes = [];      // {space, degree}
    this.anchors = null;  // f32 cap*4 (xyz, phase)
    this.meta = null;     // f32 cap*4 (birth, hue, size, seed01)
    this.state = null;    // f32 cap*2 (flag, spaceId)
    this._seeds = null;   // u32 cap
    this.edges = [];      // {src, dst, t}
    this._typed = null;    // real interlinks (colored, interactive)
    this._ambient = null;  // proximity filaments (render texture only)
    this._ambientDirty = false;
    this.spaceLayout = new Map();

    this._initGL();
    this._bindInput();
  }

  // ------------------------------------------------------------ GL setup

  _initGL() {
    const gl = this.gl;
    this.progSim = program(gl, SIM_VS, SIM_FS, 'sim', ['vPos', 'vVel']);
    this.progPoint = program(gl, POINT_VS, POINT_FS, 'point');
    this.progEdge = program(gl, EDGE_VS, EDGE_FS, 'edge');
    this.progNebula = program(gl, QUAD_VS, NEBULA_FS, 'nebula');
    this.progPick = program(gl, PICK_VS, PICK_FS, 'pick');
    this.u = {
      sim: uniformMap(gl, this.progSim),
      point: uniformMap(gl, this.progPoint),
      edge: uniformMap(gl, this.progEdge),
      nebula: uniformMap(gl, this.progNebula),
      pick: uniformMap(gl, this.progPick),
    };

    this.quad = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, this.quad);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1,-1, 1,-1, -1,1, 1,1]), gl.STATIC_DRAW);

    this.pickTex = gl.createTexture();
    this.pickFbo = gl.createFramebuffer();
    this.posTex = gl.createTexture();
    this.anchorTex = gl.createTexture();
    this.matchTex = gl.createTexture();
    this.fieldTex = gl.createTexture();
    this.orbTex = gl.createTexture();
    for (const t of [this.fieldTex, this.orbTex]) {
      gl.bindTexture(gl.TEXTURE_2D, t);
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, 1, 1, 0, gl.RGBA, gl.UNSIGNED_BYTE, new Uint8Array([8, 16, 36, 255]));
    }
    this._loadTex('/static/pillars.webp', 'fieldTex', '_fieldReady', '_fieldAspect');   // spaces backdrop
    this._loadTex('/static/southern-ring.webp', 'orbTex', '_orbReady', '_orbAspect');   // unified orb backdrop

    gl.disable(gl.DEPTH_TEST);
    gl.disable(gl.CULL_FACE);
    gl.clearColor(0.004, 0.006, 0.012, 1.0);

    this._resize();
    new ResizeObserver(() => this._resize()).observe(this.canvas);
  }

  _resize() {
    const gl = this.gl;
    const dpr = Math.min(devicePixelRatio || 1, 2);
    const w = Math.max(1, Math.round(this.canvas.clientWidth * dpr));
    const h = Math.max(1, Math.round(this.canvas.clientHeight * dpr));
    if (w === this.canvas.width && h === this.canvas.height && this.proj) return;
    this.canvas.width = w;
    this.canvas.height = h;
    this.proj = M4.persp(34, w / h, 0.1, 100);
    this.pixelScale = h / 620;

    const rgba8 = (tex, tw, th, filter) => {
      gl.bindTexture(gl.TEXTURE_2D, tex);
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, tw, th, 0, gl.RGBA, gl.UNSIGNED_BYTE, null);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, filter);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, filter);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    };
    this.pickW = Math.max(2, w >> 2);
    this.pickH = Math.max(2, h >> 2);
    rgba8(this.pickTex, this.pickW, this.pickH, gl.NEAREST);
    gl.bindFramebuffer(gl.FRAMEBUFFER, this.pickFbo);
    gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, this.pickTex, 0);
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    this.dirty = true;
  }

  _loadTex(url, texProp, readyProp, aspectProp) {
    const gl = this.gl;
    const img = new Image();
    img.onload = () => {
      gl.bindTexture(gl.TEXTURE_2D, this[texProp]);
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, img);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
      this[aspectProp] = (img.naturalWidth || 1) / (img.naturalHeight || 1);
      this[readyProp] = 1;
      this.dirty = true;
    };
    img.onerror = () => { /* keep the procedural fallback */ };
    img.src = url;
  }

  _allocParticles(cap) {
    const gl = this.gl;
    cap = Math.max(4096, cap);
    const rows = Math.ceil(cap / POS_TEX_W);
    cap = rows * POS_TEX_W;

    // release the previous generation (re-alloc happens on growth + resync)
    for (const b of [this.posA, this.posB, this.velA, this.velB,
                     this.anchorBuf, this.metaBuf, this.stateBuf]) {
      if (b) gl.deleteBuffer(b);
    }
    for (const t of [this.tfoA, this.tfoB]) {
      if (t) gl.deleteTransformFeedback(t);
    }

    const mk = (bytes, usage) => {
      const b = gl.createBuffer();
      gl.bindBuffer(gl.ARRAY_BUFFER, b);
      gl.bufferData(gl.ARRAY_BUFFER, bytes, usage);
      return b;
    };
    this.posA = mk(cap * 16, gl.DYNAMIC_COPY);
    this.posB = mk(cap * 16, gl.DYNAMIC_COPY);
    this.velA = mk(cap * 16, gl.DYNAMIC_COPY);
    this.velB = mk(cap * 16, gl.DYNAMIC_COPY);
    this.anchorBuf = mk(cap * 16, gl.DYNAMIC_DRAW);
    this.metaBuf = mk(cap * 16, gl.DYNAMIC_DRAW);
    this.stateBuf = mk(cap * 8, gl.DYNAMIC_DRAW);

    const grow = (old, n, Arr = Float32Array) => {
      const a = new Arr(cap * n);
      if (old) a.set(old.subarray(0, Math.min(old.length, cap * n)));
      return a;
    };
    this.anchors = grow(this.anchors, 4);
    this._homes = grow(this._homes, 4);
    this.meta = grow(this.meta, 4);
    this.state = grow(this.state, 2);
    this._seeds = grow(this._seeds, 1, Uint32Array);
    this.cap = cap;
    this.texRows = rows;

    const f32tex = (tex) => {
      gl.bindTexture(gl.TEXTURE_2D, tex);
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA32F, POS_TEX_W, rows, 0, gl.RGBA, gl.FLOAT, null);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    };
    f32tex(this.posTex);
    f32tex(this.anchorTex);

    // per-node match flag (R8, id-indexed) the edge shader samples; all-match
    // by default so edges show fully until a filter narrows the set
    gl.bindTexture(gl.TEXTURE_2D, this.matchTex);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.R8, POS_TEX_W, rows, 0, gl.RED, gl.UNSIGNED_BYTE, null);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    this._matchPx = new Uint8Array(POS_TEX_W * rows).fill(255);
    gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, POS_TEX_W, rows, gl.RED, gl.UNSIGNED_BYTE, this._matchPx);

    // TF objects: tfoA owns (posA, velA); tfoB owns (posB, velB).
    this.tfoA = gl.createTransformFeedback();
    gl.bindTransformFeedback(gl.TRANSFORM_FEEDBACK, this.tfoA);
    gl.bindBufferBase(gl.TRANSFORM_FEEDBACK_BUFFER, 0, this.posA);
    gl.bindBufferBase(gl.TRANSFORM_FEEDBACK_BUFFER, 1, this.velA);
    this.tfoB = gl.createTransformFeedback();
    gl.bindTransformFeedback(gl.TRANSFORM_FEEDBACK, this.tfoB);
    gl.bindBufferBase(gl.TRANSFORM_FEEDBACK_BUFFER, 0, this.posB);
    gl.bindBufferBase(gl.TRANSFORM_FEEDBACK_BUFFER, 1, this.velB);
    gl.bindTransformFeedback(gl.TRANSFORM_FEEDBACK, null);
    this._front = 0; // A holds current positions
  }

  // ------------------------------------------------------------ graph data

  setGraph(nodes, edges, spacesCount) {
    this.nodes = [];
    for (const n of nodes) {
      while (this.nodes.length <= n.id) this.nodes.push({ space: -1, degree: 0 });
      this.nodes[n.id] = { space: n.space, degree: 0 };
    }
    for (const e of edges) {
      if (this.nodes[e.src]) this.nodes[e.src].degree++;
      if (this.nodes[e.dst]) this.nodes[e.dst].degree++;
    }
    this.count = this.nodes.length;
    this._allocParticles(Math.max(4096, this.count * 2));
    this.state.fill(-1); // slots without a live node stay dead
    this._layoutSpaces(nodes);
    for (const n of nodes) {
      this._seeds[n.id] = n.seed >>> 0;
      this._writeNode(n, false);
    }
    this._refineAnchors(edges);
    for (const n of nodes) {
      const i = n.id;
      this._spawnPos(i, [this.anchors[i*4], this.anchors[i*4+1], this.anchors[i*4+2]]);
    }
    this._uploadAll();
    this.setEdges(edges);
    this.dirty = true;
    void spacesCount;
  }

  // Graph-aware layout: pull linked memories toward each other (spring to an
  // ideal edge length) while a home force preserves the seeded sphere fill —
  // real interlinks become the short, local web of the north-star instead of
  // long chords across the orb. Deterministic: fixed order, fixed iterations.
  _refineAnchors(edges) {
    if (!edges || !edges.length || !this.count) return;
    const A = this.anchors, H = this._homes;
    const IDEAL = 0.26, K = 0.10, HOME = 0.022, ITER = 140;
    for (let it = 0; it < ITER; it++) {
      for (const e of edges) {
        if (this.state[e.src*2] < -0.5 || this.state[e.dst*2] < -0.5) continue;
        const i = e.src*4, j = e.dst*4;
        let dx = A[j] - A[i], dy = A[j+1] - A[i+1], dz = A[j+2] - A[i+2];
        const L = Math.hypot(dx, dy, dz) || 1e-4;
        const f = (K * (L - IDEAL)) / L;
        dx *= f; dy *= f; dz *= f;
        A[i] += dx*0.5; A[i+1] += dy*0.5; A[i+2] += dz*0.5;
        A[j] -= dx*0.5; A[j+1] -= dy*0.5; A[j+2] -= dz*0.5;
      }
      for (let k = 0; k < this.count; k++) {
        if (this.state[k*2] < -0.5) continue;
        const i = k*4;
        A[i]   += (H[i]   - A[i])   * HOME;
        A[i+1] += (H[i+1] - A[i+1]) * HOME;
        A[i+2] += (H[i+2] - A[i+2]) * HOME;
      }
    }
  }

  _layoutSpaces(nodes) {
    // The biggest spaces get their own sphere in 'spaces' mode; the long tail
    // shares an 'other' cluster — the graceful too-many-spaces mode.
    const counts = new Map();
    for (const n of nodes) counts.set(n.space, (counts.get(n.space) || 0) + 1);
    const ranked = [...counts.entries()].sort((a, b) => b[1] - a[1]);
    const MAXC = 21;
    const clusters = ranked.slice(0, MAXC);
    const maxN = clusters.length ? clusters[0][1] : 1;
    this.spaceLayout.clear();
    const total = clusters.length;
    const golden = Math.PI * (3 - Math.sqrt(5));
    const place = (i, count) => {
      const y = total <= 1 ? 0 : 1 - (2 * (i + 0.5)) / total;
      const rr = Math.sqrt(Math.max(0, 1 - y * y));
      const th = golden * i;
      return {
        // spread the space centres wider and keep each cluster tighter so the
        // spheres read as distinct clumps instead of one loose scatter
        center: [Math.cos(th) * rr * 2.0, y * 1.55, Math.sin(th) * rr * 2.0],
        radius: 0.22 + 0.46 * Math.cbrt(count / maxN),
      };
    };
    clusters.forEach(([sid, c], i) => this.spaceLayout.set(sid, place(i, c)));
    if (ranked.length > MAXC) {
      // the long tail forms the undifferentiated core; named spaces orbit it
      this.spaceLayout.set(-1, { center: [0, 0, 0], radius: 0.85 });
    }
  }

  _anchorFor(id, seed, space) {
    const rnd = seededRand(seed);
    const u = rnd(), v = rnd(), w = rnd();
    const th = 6.28318 * u;
    const z = 2 * v - 1;
    const rr = Math.sqrt(Math.max(0, 1 - z * z));
    const deg = this.nodes[id] ? this.nodes[id].degree : 0;
    const dir = [rr * Math.cos(th), z, rr * Math.sin(th)];
    let r = Math.pow(w, 0.55); // center-weighted fill, like the north-star
    r *= 1 - 0.30 * Math.min(1, deg / 9); // hubs gravitate to the heart
    if (this.mode === 'unified') {
      const R = 1.15;
      return [dir[0]*r*R, dir[1]*r*R, dir[2]*r*R];
    }
    const lay = this.spaceLayout.get(space) || this.spaceLayout.get(-1)
      || { center: [0, 0, 0], radius: 1.2 };
    return [
      lay.center[0] + dir[0]*r*lay.radius,
      lay.center[1] + dir[1]*r*lay.radius,
      lay.center[2] + dir[2]*r*lay.radius,
    ];
  }

  _writeNode(node, fresh) {
    const i = node.id;
    const a = this._anchorFor(i, node.seed, node.space);
    this.anchors[i*4] = this._homes[i*4] = a[0];
    this.anchors[i*4+1] = this._homes[i*4+1] = a[1];
    this.anchors[i*4+2] = this._homes[i*4+2] = a[2];
    this.anchors[i*4+3] = (node.seed % 6283) / 1000;
    const deg = this.nodes[i] ? this.nodes[i].degree : 0;
    const rnd = seededRand(node.seed ^ 0x9e3779b9);
    this.meta[i*4] = fresh ? this.time : -1;
    this.meta[i*4+1] = node.kind === 'code' ? 1 : 0;
    // wide size spread so degree reads: small nodes stay small, hubs bulk up
    this.meta[i*4+2] = 1.6 + Math.min(6.4, Math.pow(deg, 0.62) * 1.65) + rnd() * 0.6;
    this.meta[i*4+3] = rnd();
    this.state[i*2] = 1;
    this.state[i*2+1] = node.space;
  }

  _spawnPos(i, p) {
    const gl = this.gl;
    const pos = new Float32Array([p[0], p[1], p[2], 1]);
    const vel = new Float32Array([0, 0, 0, 0]);
    for (const b of [this.posA, this.posB]) {
      gl.bindBuffer(gl.ARRAY_BUFFER, b);
      gl.bufferSubData(gl.ARRAY_BUFFER, i * 16, pos);
    }
    for (const b of [this.velA, this.velB]) {
      gl.bindBuffer(gl.ARRAY_BUFFER, b);
      gl.bufferSubData(gl.ARRAY_BUFFER, i * 16, vel);
    }
  }

  _uploadAll() {
    const gl = this.gl;
    gl.bindBuffer(gl.ARRAY_BUFFER, this.anchorBuf);
    gl.bufferSubData(gl.ARRAY_BUFFER, 0, this.anchors);
    gl.bindBuffer(gl.ARRAY_BUFFER, this.metaBuf);
    gl.bufferSubData(gl.ARRAY_BUFFER, 0, this.meta);
    gl.bindBuffer(gl.ARRAY_BUFFER, this.stateBuf);
    gl.bufferSubData(gl.ARRAY_BUFFER, 0, this.state);
    gl.bindTexture(gl.TEXTURE_2D, this.anchorTex);
    gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, POS_TEX_W, this.texRows, gl.RGBA, gl.FLOAT, this.anchors);
  }

  _uploadNode(i) {
    const gl = this.gl;
    gl.bindBuffer(gl.ARRAY_BUFFER, this.anchorBuf);
    gl.bufferSubData(gl.ARRAY_BUFFER, i * 16, this.anchors.subarray(i*4, i*4+4));
    gl.bindBuffer(gl.ARRAY_BUFFER, this.metaBuf);
    gl.bufferSubData(gl.ARRAY_BUFFER, i * 16, this.meta.subarray(i*4, i*4+4));
    gl.bindBuffer(gl.ARRAY_BUFFER, this.stateBuf);
    gl.bufferSubData(gl.ARRAY_BUFFER, i * 8, this.state.subarray(i*2, i*2+2));
    gl.bindTexture(gl.TEXTURE_2D, this.anchorTex);
    gl.texSubImage2D(gl.TEXTURE_2D, 0, i % POS_TEX_W, (i / POS_TEX_W) | 0, 1, 1,
      gl.RGBA, gl.FLOAT, this.anchors.subarray(i*4, i*4+4));
  }

  addNode(node) {
    const i = node.id;
    while (this.nodes.length <= i) this.nodes.push({ space: -1, degree: 0 });
    this.nodes[i] = { space: node.space, degree: this.nodes[i].degree || 0 };
    if (i >= this.cap) {
      this._allocParticles(this.cap * 2);
      this._uploadAll();
      for (let k = 0; k < this.count; k++) {
        this._spawnPos(k, [this.anchors[k*4], this.anchors[k*4+1], this.anchors[k*4+2]]);
      }
      this.setEdges(this.edges);
    }
    this.count = Math.max(this.count, i + 1);
    this._seeds[i] = node.seed >>> 0;
    this._writeNode(node, true);
    // born at the heart of its cluster, springs carry it outward
    const lay = this.mode === 'spaces'
      ? (this.spaceLayout.get(node.space) || this.spaceLayout.get(-1) || { center: [0, 0, 0] })
      : { center: [0, 0, 0] };
    this._spawnPos(i, lay.center);
    this._uploadNode(i);
    this.pulse(i);
    this._ambientDirty = true;
    this.dirty = true;
  }

  updateNode(node) {
    const i = node.id;
    if (i >= this.count) return this.addNode(node);
    this.nodes[i].space = node.space;
    this.meta[i*4] = this.time - 0.45; // small re-pop on a new revision
    this._uploadNode(i);
    this.pulse(i);
    this.dirty = true;
  }

  removeNode(id, edges) {
    if (id < this.count) {
      this.state[id*2] = -1;
      this._uploadNode(id);
    }
    this.setEdges(edges);
    this.dirty = true;
  }

  bumpDegree(id) {
    if (this.nodes[id]) this.nodes[id].degree++;
  }

  /// Fill one of the two edge-line vertex-buffer sets (typed | ambient).
  _fillEdgeStore(store, list) {
    const gl = this.gl;
    const n = list.length * 2;
    if (!store.buf || n > store.cap) {
      store.cap = Math.max(8192, n * 2);
      const mk = (comps) => {
        const b = gl.createBuffer();
        gl.bindBuffer(gl.ARRAY_BUFFER, b);
        gl.bufferData(gl.ARRAY_BUFFER, store.cap * comps * 4, gl.DYNAMIC_DRAW);
        return b;
      };
      for (const b of Object.values(store.buf || {})) gl.deleteBuffer(b);
      store.buf = { idx: mk(1), other: mk(1), t: mk(1), type: mk(1), spaces: mk(2) };
    }
    const idx = new Float32Array(n), other = new Float32Array(n), tt = new Float32Array(n),
          ty = new Float32Array(n), sp = new Float32Array(n * 2);
    list.forEach((e, k) => {
      const sA = this.state[e.src*2+1], sB = this.state[e.dst*2+1];
      idx[k*2] = e.src;   other[k*2] = e.dst;   tt[k*2] = 0;   ty[k*2] = e.t;
      idx[k*2+1] = e.dst; other[k*2+1] = e.src; tt[k*2+1] = 1; ty[k*2+1] = e.t;
      sp[k*4] = sA; sp[k*4+1] = sB; sp[k*4+2] = sA; sp[k*4+3] = sB;
    });
    const up = (buf, arr) => {
      gl.bindBuffer(gl.ARRAY_BUFFER, buf);
      gl.bufferSubData(gl.ARRAY_BUFFER, 0, arr);
    };
    up(store.buf.idx, idx);
    up(store.buf.other, other);
    up(store.buf.t, tt);
    up(store.buf.type, ty);
    up(store.buf.spaces, sp);
    store.count = n;
    this.dirty = true;
  }

  setEdges(edges) {
    this.edges = edges;
    this._typed = this._typed || { buf: null, cap: 0, count: 0 };
    this._fillEdgeStore(this._typed, edges);
    this._ambientDirty = true;
  }

  // The north-star's lattice: k-nearest proximity filaments over the layout.
  // A render treatment of the point cloud (like the nebula) — anonymous,
  // faint, non-interactive; the *typed* interlinks above it are the data.
  _buildAmbient() {
    this._ambientDirty = false;
    this._ambient = this._ambient || { buf: null, cap: 0, count: 0 };
    const N = this.count;
    const A = this.anchors;
    const K = N > 6000 ? 2 : 3;
    const CELL = 0.34, MAXD2 = 0.70 * 0.70;
    const buckets = new Map();
    const keyOf = (x, y, z) =>
      `${Math.floor(x / CELL)},${Math.floor(y / CELL)},${Math.floor(z / CELL)}`;
    for (let i = 0; i < N; i++) {
      if (this.state[i*2] < -0.5) continue;
      const k = keyOf(A[i*4], A[i*4+1], A[i*4+2]);
      let b = buckets.get(k);
      if (!b) buckets.set(k, (b = []));
      b.push(i);
    }
    const out = [];
    const seen = new Set();
    const best = [];
    for (let i = 0; i < N; i++) {
      if (this.state[i*2] < -0.5) continue;
      const cx = Math.floor(A[i*4] / CELL), cy = Math.floor(A[i*4+1] / CELL),
            cz = Math.floor(A[i*4+2] / CELL);
      best.length = 0;
      for (let dx = -1; dx <= 1; dx++) for (let dy = -1; dy <= 1; dy++) for (let dz = -1; dz <= 1; dz++) {
        const b = buckets.get(`${cx+dx},${cy+dy},${cz+dz}`);
        if (!b) continue;
        for (const j of b) {
          if (j === i) continue;
          const ddx = A[i*4]-A[j*4], ddy = A[i*4+1]-A[j*4+1], ddz = A[i*4+2]-A[j*4+2];
          const d2 = ddx*ddx + ddy*ddy + ddz*ddz;
          if (d2 > MAXD2) continue;
          if (best.length < K) {
            best.push([d2, j]);
            best.sort((a, b2) => a[0] - b2[0]);
          } else if (d2 < best[best.length-1][0]) {
            best[best.length-1] = [d2, j];
            best.sort((a, b2) => a[0] - b2[0]);
          }
        }
      }
      for (const [, j] of best) {
        const key = i < j ? i * 1048576 + j : j * 1048576 + i;
        if (seen.has(key)) continue;
        seen.add(key);
        out.push({ src: i, dst: j, t: 7 });
      }
    }
    this._fillEdgeStore(this._ambient, out);
  }

  addEdge(edge) {
    const gl = this.gl;
    // a young memory attaches beside what it links to — the live counterpart
    // of the snapshot's graph-aware layout
    const young = (id) => this.meta[id*4] > 0 && this.time - this.meta[id*4] < 120;
    let move = -1, to = -1;
    if (young(edge.src) && !young(edge.dst)) { move = edge.src; to = edge.dst; }
    else if (young(edge.dst) && !young(edge.src)) { move = edge.dst; to = edge.src; }
    if (move >= 0 && this.state[move*2] > -0.5) {
      const m = move*4, o = to*4;
      let dx = this.anchors[m] - this.anchors[o],
          dy = this.anchors[m+1] - this.anchors[o+1],
          dz = this.anchors[m+2] - this.anchors[o+2];
      const L = Math.hypot(dx, dy, dz) || 1;
      const s = 0.26 / L;
      this.anchors[m]   = this._homes[m]   = this.anchors[o]   + dx*s;
      this.anchors[m+1] = this._homes[m+1] = this.anchors[o+1] + dy*s;
      this.anchors[m+2] = this._homes[m+2] = this.anchors[o+2] + dz*s;
      this._uploadNode(move);
    }
    const st = this._typed;
    if (!st || !st.buf || (this.edges.length + 1) * 2 > st.cap) {
      this.edges.push(edge);
      return this.setEdges(this.edges);
    }
    const k = this.edges.length;
    this.edges.push(edge);
    const sA = this.state[edge.src*2+1], sB = this.state[edge.dst*2+1];
    const up = (buf, comps, arr) => {
      gl.bindBuffer(gl.ARRAY_BUFFER, buf);
      gl.bufferSubData(gl.ARRAY_BUFFER, k * 2 * comps * 4, new Float32Array(arr));
    };
    up(st.buf.idx, 1, [edge.src, edge.dst]);
    up(st.buf.other, 1, [edge.dst, edge.src]);
    up(st.buf.t, 1, [0, 1]);
    up(st.buf.type, 1, [edge.t, edge.t]);
    up(st.buf.spaces, 2, [sA, sB, sA, sB]);
    st.count = (k + 1) * 2;
    this._ambientDirty = true;
    this.dirty = true;
  }

  setMode(mode) {
    if (mode === this.mode) return;
    this.mode = mode;
    // reframe: the constellation is wider than the unified sphere
    this.cam.tyaw = this.cam.yaw;
    this.cam.tpitch = this.cam.pitch;
    this.cam.tdist = mode === 'spaces' ? 7.6 : 5.2;
    for (let i = 0; i < this.count; i++) {
      if (this.state[i*2] < -0.5) continue;
      const a = this._anchorFor(i, this._seeds[i], this.nodes[i].space);
      this.anchors[i*4] = this._homes[i*4] = a[0];
      this.anchors[i*4+1] = this._homes[i*4+1] = a[1];
      this.anchors[i*4+2] = this._homes[i*4+2] = a[2];
    }
    this._refineAnchors(this.edges);
    this._uploadAll();
    this._buildAmbient(); // anchors moved — refresh the lattice immediately
    this.dirty = true;
  }

  setMatch(flags) {
    this.searchActive = flags ? 1 : 0;
    for (let i = 0; i < this.count; i++) {
      if (this.state[i*2] < -0.5) continue;
      this.state[i*2] = flags ? (flags[i] ? 1 : 0) : 1;
    }
    const gl = this.gl;
    gl.bindBuffer(gl.ARRAY_BUFFER, this.stateBuf);
    gl.bufferSubData(gl.ARRAY_BUFFER, 0, this.state.subarray(0, this.count * 2));
    // mirror the per-node match into the edge shader's id-indexed lookup, so
    // edges within the matched set stay lit while the rest fade
    const m = this._matchPx;
    m.fill(255);
    if (flags) for (let i = 0; i < this.count; i++) if (!flags[i]) m[i] = 0;
    gl.bindTexture(gl.TEXTURE_2D, this.matchTex);
    gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, POS_TEX_W, this.texRows, gl.RED, gl.UNSIGNED_BYTE, m);
    this.dirty = true;
  }

  setSpaceHighlight(spaceId) {
    this.spaceHi = spaceId == null ? -1 : spaceId;
    this.dirty = true;
  }

  select(id) {
    this.sel = id == null ? -1 : id;
    if (this.sel >= 0) this.pulse(this.sel);
    this.dirty = true;
  }

  pulse(id) {
    this.pulseNode = id;
    this.pulseT = this.time;
    this.dirty = true;
  }

  focus(id) {
    if (id < 0 || id >= this.count) return;
    const x = this.anchors[id*4], y = this.anchors[id*4+1], z = this.anchors[id*4+2];
    const len = Math.hypot(x, y, z) || 1;
    this.cam.tyaw = Math.atan2(x, z);
    this.cam.tpitch = Math.max(-1.1, Math.min(1.1, Math.asin(y / len) * 0.85));
    this.cam.tdist = Math.max(3.4, Math.min(7.5, len + 3.2));
    this.select(id);
  }

  // ------------------------------------------------------------ input

  _bindInput() {
    const c = this.canvas;
    let dragging = false, lx = 0, ly = 0, moved = 0;
    c.addEventListener('pointerdown', (e) => {
      dragging = true;
      moved = 0;
      lx = e.clientX;
      ly = e.clientY;
      c.setPointerCapture(e.pointerId);
      this.cam.lastInput = this.time;
    });
    c.addEventListener('pointermove', (e) => {
      if (!dragging) return;
      const dx = e.clientX - lx, dy = e.clientY - ly;
      lx = e.clientX;
      ly = e.clientY;
      moved += Math.abs(dx) + Math.abs(dy);
      this.cam.vyaw = -dx * 0.005;
      this.cam.vpitch = dy * 0.004;
      this.cam.yaw += this.cam.vyaw;
      this.cam.pitch = Math.max(-1.2, Math.min(1.2, this.cam.pitch + this.cam.vpitch));
      this.cam.tyaw = this.cam.tpitch = this.cam.tdist = null;
      this.cam.lastInput = this.time;
      this.dirty = true;
    });
    c.addEventListener('pointerup', (e) => {
      dragging = false;
      this.cam.lastInput = this.time;
      if (moved < 6 && this.onclickNode) this.onclickNode(this.pick(e.clientX, e.clientY));
    });
    c.addEventListener('wheel', (e) => {
      e.preventDefault();
      this.cam.dist = Math.max(2.6, Math.min(11, this.cam.dist * Math.exp(e.deltaY * 0.0012)));
      this.cam.tdist = null;
      this.cam.lastInput = this.time;
      this.dirty = true;
    }, { passive: false });
  }

  pick(clientX, clientY) {
    const gl = this.gl;
    if (!this.view || this.count === 0) return -1;
    const rect = this.canvas.getBoundingClientRect();
    const x = ((clientX - rect.left) / rect.width) * this.pickW;
    const y = (1 - (clientY - rect.top) / rect.height) * this.pickH;
    this._renderPick();
    const px = new Uint8Array(4);
    gl.bindFramebuffer(gl.FRAMEBUFFER, this.pickFbo);
    gl.readPixels(
      Math.max(0, Math.min(this.pickW - 1, x | 0)),
      Math.max(0, Math.min(this.pickH - 1, y | 0)),
      1, 1, gl.RGBA, gl.UNSIGNED_BYTE, px);
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    if (px[0] === 255 && px[1] === 255 && px[2] === 255) return -1;
    const id = px[0] | (px[1] << 8) | (px[2] << 16);
    return id < this.count && this.state[id*2] > -0.5 ? id : -1;
  }

  _renderPick() {
    const gl = this.gl;
    gl.bindFramebuffer(gl.FRAMEBUFFER, this.pickFbo);
    gl.viewport(0, 0, this.pickW, this.pickH);
    gl.clearColor(1, 1, 1, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);
    gl.disable(gl.BLEND);
    gl.useProgram(this.progPick);
    const u = this.u.pick;
    gl.uniformMatrix4fv(u.uProj, false, this.proj);
    gl.uniformMatrix4fv(u.uView, false, this.view);
    gl.uniform2f(u.uViewport, this.pickW, this.pickH);
    gl.uniform1f(u.uTime, this.time);
    gl.uniform1f(u.uSearch, this.searchActive);
    gl.uniform1f(u.uStateless, this.stateless);
    gl.uniform1f(u.uPixelScale, this.pixelScale / 4);
    this._bindPointAttribs(this.progPick);
    gl.drawArraysInstanced(gl.TRIANGLE_STRIP, 0, 4, this.count);
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    gl.clearColor(0.004, 0.006, 0.012, 1.0);
  }

  // ------------------------------------------------------------ frame

  _attrib(prog, name, buf, comps, divisor) {
    const gl = this.gl;
    const loc = gl.getAttribLocation(prog, name);
    if (loc < 0) return;
    gl.bindBuffer(gl.ARRAY_BUFFER, buf);
    gl.enableVertexAttribArray(loc);
    gl.vertexAttribPointer(loc, comps, gl.FLOAT, false, 0, 0);
    gl.vertexAttribDivisor(loc, divisor);
  }

  _bindPointAttribs(prog) {
    const front = this._front === 0 ? this.posA : this.posB;
    this._attrib(prog, 'aCorner', this.quad, 2, 0);
    this._attrib(prog, 'iPos', front, 4, 1);
    this._attrib(prog, 'iAnchor', this.anchorBuf, 4, 1);
    this._attrib(prog, 'iMeta', this.metaBuf, 4, 1);
    this._attrib(prog, 'iState', this.stateBuf, 2, 1);
  }

  frame(now) {
    const gl = this.gl;
    if (!this.startT) this.startT = now;
    const t = (now - this.startT) / 1000;
    const dt = Math.min(1 / 30, Math.max(0.0001, t - this.time));
    this.time = t;

    this._fpsAcc += dt;
    this._fpsN++;
    if (this._fpsAcc >= 0.5) {
      this.fps = Math.round(this._fpsN / this._fpsAcc);
      this._fpsAcc = 0;
      this._fpsN = 0;
      if (this.onfps) this.onfps(this.fps);
    }

    // camera
    const cam = this.cam;
    if (cam.tyaw != null) {
      const k = 1 - Math.exp(-dt * 5);
      let dy = cam.tyaw - cam.yaw;
      dy = Math.atan2(Math.sin(dy), Math.cos(dy));
      cam.yaw += dy * k;
      cam.pitch += (cam.tpitch - cam.pitch) * k;
      cam.dist += (cam.tdist - cam.dist) * k;
      if (Math.abs(dy) < 0.01 && Math.abs(cam.tpitch - cam.pitch) < 0.01
          && Math.abs(cam.tdist - cam.dist) < 0.05) {
        cam.tyaw = cam.tpitch = cam.tdist = null;
      }
      this.dirty = true;
    } else {
      cam.vyaw *= Math.exp(-dt * 3);
      cam.vpitch *= Math.exp(-dt * 3);
      if (Math.abs(cam.vyaw) > 1e-4 || Math.abs(cam.vpitch) > 1e-4) {
        cam.yaw += cam.vyaw;
        cam.pitch = Math.max(-1.2, Math.min(1.2, cam.pitch + cam.vpitch));
        this.dirty = true;
      }
      if (!this.reduce && t - cam.lastInput > 2.5) {
        cam.yaw += dt * cam.auto;
        this.dirty = true;
      }
    }
    this.view = M4.mul(
      M4.mul(M4.translate(0, 0.22, -cam.dist), M4.rotX(cam.pitch)),
      M4.rotY(cam.yaw));

    if (this.reduce && !this.dirty) return;

    // 1. simulation (transform feedback)
    if (!this.stateless && this.count > 0) {
      const readPos = this._front === 0 ? this.posA : this.posB;
      const readVel = this._front === 0 ? this.velA : this.velB;
      const writeTfo = this._front === 0 ? this.tfoB : this.tfoA;
      gl.useProgram(this.progSim);
      gl.uniform1f(this.u.sim.uDt, dt);
      gl.uniform1f(this.u.sim.uTime, t);
      this._attrib(this.progSim, 'aPos', readPos, 4, 0);
      this._attrib(this.progSim, 'aVel', readVel, 4, 0);
      this._attrib(this.progSim, 'aAnchor', this.anchorBuf, 4, 0);
      gl.enable(gl.RASTERIZER_DISCARD);
      gl.bindTransformFeedback(gl.TRANSFORM_FEEDBACK, writeTfo);
      gl.beginTransformFeedback(gl.POINTS);
      gl.drawArrays(gl.POINTS, 0, this.count);
      gl.endTransformFeedback();
      gl.disable(gl.RASTERIZER_DISCARD);
      // Detach the freshly-written buffer from the *current* TF object by
      // binding the other one, then GPU-copy positions into the texture the
      // edge shader samples.
      const written = this._front === 0 ? this.posB : this.posA;
      gl.bindTransformFeedback(gl.TRANSFORM_FEEDBACK, this._front === 0 ? this.tfoA : this.tfoB);
      gl.bindBuffer(gl.PIXEL_UNPACK_BUFFER, written);
      gl.bindTexture(gl.TEXTURE_2D, this.posTex);
      gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, POS_TEX_W, this.texRows, gl.RGBA, gl.FLOAT, 0);
      gl.bindBuffer(gl.PIXEL_UNPACK_BUFFER, null);
      gl.bindTransformFeedback(gl.TRANSFORM_FEEDBACK, null);
      this._front = 1 - this._front;
      if (!this._healthChecked && t > 1.5) this._healthCheck();
    }

    const W = this.canvas.width, H = this.canvas.height;

    // 2. nebula — full resolution, straight to the backbuffer (the
    // north-star's filament detail dies under half-res upsampling)
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    gl.viewport(0, 0, W, H);
    gl.clear(gl.COLOR_BUFFER_BIT);
    gl.disable(gl.BLEND);
    gl.useProgram(this.progNebula);
    const c0 = M4.apply(M4.mul(this.proj, this.view), 0, 0, 0);
    gl.uniform2f(this.u.nebula.uCenter,
      c0[3] !== 0 ? c0[0] / c0[3] : 0,
      c0[3] !== 0 ? c0[1] / c0[3] : 0);
    gl.uniform1f(this.u.nebula.uAspect, W / H);
    gl.uniform1f(this.u.nebula.uTime, this.reduce ? 40.0 : t);
    // in 'spaces' mode the constellation fans out into separate clusters, so
    // the single central orb-glow is dimmed to a wide ambient wash instead of
    // a dominant blob
    // ease the unified<->spaces backdrop so the mode switch crossfades instead
    // of popping (nodes already spring + camera eases; this matches them)
    const target = this.mode === 'spaces' ? 1 : 0;
    this._spacesMix += (target - this._spacesMix) * (1 - Math.exp(-dt * 2.4));
    const sm = this._spacesMix;
    if (sm > 0.001 && sm < 0.999) this.dirty = true;
    gl.uniform1f(this.u.nebula.uSpaces, sm);
    gl.uniform1f(this.u.nebula.uInt, 0.72 + 0.10 * sm);
    gl.uniform1f(this.u.nebula.uRad, (1.05 + 0.50 * sm) * (5.2 / cam.dist));
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, this.fieldTex);
    gl.uniform1i(this.u.nebula.uFieldTex, 0);
    gl.uniform1f(this.u.nebula.uFieldReady, this._fieldReady);
    gl.uniform1f(this.u.nebula.uFieldAspect, this._fieldAspect);
    gl.activeTexture(gl.TEXTURE1);
    gl.bindTexture(gl.TEXTURE_2D, this.orbTex);
    gl.uniform1i(this.u.nebula.uOrbTex, 1);
    gl.uniform1f(this.u.nebula.uOrbReady, this._orbReady);
    gl.uniform1f(this.u.nebula.uOrbAspect, this._orbAspect);
    this._attrib(this.progNebula, 'aP', this.quad, 2, 0);
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);

    gl.enable(gl.BLEND);
    gl.blendFunc(gl.ONE, gl.ONE);

    // 4. edge web — ambient proximity filaments beneath, typed interlinks on top
    if (this._ambientDirty && t - (this._ambientAt || -9) > 2.0) {
      this._buildAmbient();
      this._ambientAt = t;
    }
    if ((this._ambient && this._ambient.count) || (this._typed && this._typed.count)) {
      gl.useProgram(this.progEdge);
      const u = this.u.edge;
      gl.uniformMatrix4fv(u.uProj, false, this.proj);
      gl.uniformMatrix4fv(u.uView, false, this.view);
      gl.uniform1f(u.uTime, t);
      gl.uniform1f(u.uSearch, this.searchActive);
      gl.uniform1f(u.uSel, this.sel);
      gl.uniform1f(u.uSpaceHi, this.spaceHi);
      gl.uniform1f(u.uStateless, this.stateless);
      gl.uniform4f(u.uPulse, this.pulseNode, this.pulseT, 0, 0);
      gl.uniform3fv(u.uTypeCol, TYPE_COLORS);
      gl.activeTexture(gl.TEXTURE0);
      gl.bindTexture(gl.TEXTURE_2D, this.posTex);
      gl.uniform1i(u.uPosTex, 0);
      gl.activeTexture(gl.TEXTURE1);
      gl.bindTexture(gl.TEXTURE_2D, this.anchorTex);
      gl.uniform1i(u.uAnchorTex, 1);
      gl.activeTexture(gl.TEXTURE2);
      gl.bindTexture(gl.TEXTURE_2D, this.matchTex);
      gl.uniform1i(u.uMatchTex, 2);
      const drawStore = (store, ambient) => {
        if (!store || !store.count) return;
        gl.uniform1f(u.uAmbient, ambient);
        this._attrib(this.progEdge, 'aIdx', store.buf.idx, 1, 0);
        this._attrib(this.progEdge, 'aOther', store.buf.other, 1, 0);
        this._attrib(this.progEdge, 'aT', store.buf.t, 1, 0);
        this._attrib(this.progEdge, 'aType', store.buf.type, 1, 0);
        this._attrib(this.progEdge, 'aSpaces', store.buf.spaces, 2, 0);
        gl.drawArrays(gl.LINES, 0, store.count);
      };
      drawStore(this._ambient, 1);
      drawStore(this._typed, 0);
    }

    // 5. points
    if (this.count > 0) {
      gl.useProgram(this.progPoint);
      const u = this.u.point;
      gl.uniformMatrix4fv(u.uProj, false, this.proj);
      gl.uniformMatrix4fv(u.uView, false, this.view);
      gl.uniform2f(u.uViewport, W, H);
      gl.uniform1f(u.uTime, t);
      gl.uniform1f(u.uSearch, this.searchActive);
      gl.uniform1f(u.uSel, this.sel);
      gl.uniform1f(u.uSpaceHi, this.spaceHi);
      gl.uniform1f(u.uStateless, this.stateless);
      gl.uniform1f(u.uPixelScale, this.pixelScale);
      this._bindPointAttribs(this.progPoint);
      gl.drawArraysInstanced(gl.TRIANGLE_STRIP, 0, 4, this.count);
    }

    gl.disable(gl.BLEND);
    this.dirty = false;

    if (!this.verified && t > 1.2 && this.count > 0) this._verifyRender();
  }

  // M2 acceptance à la the DECISION.VAULT prototype: prove real luminance is
  // on screen by reading center pixels back right after the draw.
  _verifyRender() {
    const gl = this.gl;
    const px = new Uint8Array(4 * 9);
    gl.readPixels((this.canvas.width >> 1) - 1, (this.canvas.height >> 1) - 1, 3, 3,
      gl.RGBA, gl.UNSIGNED_BYTE, px);
    let lum = 0;
    for (let i = 0; i < 9; i++) lum += (px[i*4] + px[i*4+1] + px[i*4+2]) / 3;
    lum /= 9;
    this.verified = true;
    console.log(`[brain] render check: center luminance ${lum.toFixed(1)} ${lum > 2 ? '✓ glow verified' : '✗ dark — check pipeline'}`);
  }

  // If TF/PBO misbehaves on this driver (NaN or runaway positions), flip to
  // the stateless path — visually equivalent, zero per-point CPU either way.
  _healthCheck() {
    const gl = this.gl;
    this._healthChecked = true;
    try {
      const front = this._front === 0 ? this.posA : this.posB;
      gl.bindTransformFeedback(gl.TRANSFORM_FEEDBACK, this._front === 0 ? this.tfoB : this.tfoA);
      const out = new Float32Array(4);
      gl.bindBuffer(gl.ARRAY_BUFFER, front);
      gl.getBufferSubData(gl.ARRAY_BUFFER, 0, out);
      gl.bindTransformFeedback(gl.TRANSFORM_FEEDBACK, null);
      const bad = !Number.isFinite(out[0]) || !Number.isFinite(out[1])
        || Math.hypot(out[0], out[1], out[2]) > 50;
      if (bad) {
        console.warn('[brain] transform-feedback health check failed — stateless drift fallback engaged');
        this.stateless = 1;
      } else {
        console.log('[brain] transform-feedback sim healthy ✓');
      }
    } catch (e) {
      console.warn('[brain] TF health check error — stateless drift fallback engaged', e);
      this.stateless = 1;
    }
  }

  start() {
    if (this._raf) return;
    const loop = (now) => {
      this._raf = requestAnimationFrame(loop);
      try {
        this.frame(now);
      } catch (e) {
        console.error('[brain] frame error', e);
        cancelAnimationFrame(this._raf);
        this._raf = null;
      }
    };
    this._raf = requestAnimationFrame(loop);
  }

  stop() {
    if (this._raf) cancelAnimationFrame(this._raf);
    this._raf = null;
  }
}
