// Optional WebGL2 interaction preview. CPU confirms every saved version and owns export.
import { el } from '../../../core/dom.js';
import { t } from '../../../core/i18n.js';
import { api } from '../../../core/api.js';
export function makeGpuPreview({ layer, getInfo, allowed }) {
  const canvas = el('canvas.develop-gpu', { hidden: true, 'aria-label': t('dev.gpu') });
  const badge = el('span.develop-gpu-badge', { hidden: true, text: t('dev.gpu') });
  layer.append(canvas, badge);
  let gl = null, program, sourceTex, lutTex, buffer, source = null, sourceUrl = '', sourcePromise = null, dead = false;
  let serial = 0, latest = null, running = false, timer = 0;
  function hide() { canvas.hidden = true; badge.hidden = true; }
  function clear() { serial++; latest = null; clearTimeout(timer); hide(); }
  function init() {
    if (gl) return true; if (dead) return false;
    gl = canvas.getContext('webgl2', { alpha: false, preserveDrawingBuffer: false });
    if (!gl) { dead = true; return false; }
    const shader = (type, text) => { const s = gl.createShader(type); gl.shaderSource(s, text); gl.compileShader(s); if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) { gl.deleteShader(s); throw new Error('shader'); } return s; };
    try {
      const vs = shader(gl.VERTEX_SHADER, '#version 300 es\nin vec2 position;out vec2 uv;void main(){uv=vec2((position.x+1.)*.5,(1.-position.y)*.5);gl_Position=vec4(position,0.,1.);}');
      const fs = shader(gl.FRAGMENT_SHADER, '#version 300 es\nprecision highp float;precision highp sampler3D;uniform sampler2D photo;uniform sampler3D lookup;uniform float size;in vec2 uv;out vec4 outColor;void main(){vec3 c=texture(photo,uv).rgb;vec3 p=(c*(size-1.)+.5)/size;outColor=vec4(texture(lookup,p).rgb,1.);}');
      program = gl.createProgram(); gl.attachShader(program, vs); gl.attachShader(program, fs); gl.linkProgram(program); gl.deleteShader(vs); gl.deleteShader(fs);
      if (!gl.getProgramParameter(program, gl.LINK_STATUS)) throw new Error('program');
      buffer = gl.createBuffer(); gl.bindBuffer(gl.ARRAY_BUFFER, buffer); gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1,-1,1,-1,-1,1,1,1]), gl.STATIC_DRAW);
      gl.useProgram(program); const a = gl.getAttribLocation(program, 'position'); gl.enableVertexAttribArray(a); gl.vertexAttribPointer(a, 2, gl.FLOAT, false, 0, 0);
      gl.uniform1i(gl.getUniformLocation(program, 'photo'), 0); gl.uniform1i(gl.getUniformLocation(program, 'lookup'), 1);
      sourceTex = gl.createTexture(); lutTex = gl.createTexture(); return true;
    } catch { dead = true; release(); return false; }
  }
  function release() { if (!gl) return; if (program) gl.deleteProgram(program); if (sourceTex) gl.deleteTexture(sourceTex); if (lutTex) gl.deleteTexture(lutTex); if (buffer) gl.deleteBuffer(buffer); gl = null; }
  const params = target => { gl.texParameteri(target,gl.TEXTURE_MIN_FILTER,gl.LINEAR);gl.texParameteri(target,gl.TEXTURE_MAG_FILTER,gl.LINEAR);gl.texParameteri(target,gl.TEXTURE_WRAP_S,gl.CLAMP_TO_EDGE);gl.texParameteri(target,gl.TEXTURE_WRAP_T,gl.CLAMP_TO_EDGE);if(target===gl.TEXTURE_3D)gl.texParameteri(target,gl.TEXTURE_WRAP_R,gl.CLAMP_TO_EDGE); };
  async function load(url) {
    if (url === sourceUrl && sourcePromise) return sourcePromise;
    sourceUrl = url; source = null;
    const img = new Image(); const token = url;
    sourcePromise = new Promise((resolve,reject) => { img.onload=()=>{if(token===sourceUrl)source=img;resolve(img);};img.onerror=reject;img.src=url; });
    return sourcePromise;
  }
  async function run() {
    if (running || !latest || !init()) return;
    const request = latest; latest = null; running = true;
    const { token, id, ops, url } = request;
    try {
      const [table, img] = await Promise.all([api.adjustGpu(id, ops), load(url)]);
      if (token !== serial || !allowed() || id !== getInfo()?.id || !table.supported) return;
      if (!gl || gl.isContextLost()) return;
      const max = Math.min(2048,gl.getParameter(gl.MAX_TEXTURE_SIZE)), scale = Math.min(1,max/Math.max(img.naturalWidth,img.naturalHeight));
      canvas.width=Math.max(1,Math.round(img.naturalWidth*scale));canvas.height=Math.max(1,Math.round(img.naturalHeight*scale));
      gl.viewport(0,0,canvas.width,canvas.height);gl.useProgram(program);
      gl.activeTexture(gl.TEXTURE0);gl.bindTexture(gl.TEXTURE_2D,sourceTex);params(gl.TEXTURE_2D);gl.texImage2D(gl.TEXTURE_2D,0,gl.RGB,gl.RGB,gl.UNSIGNED_BYTE,img);
      gl.activeTexture(gl.TEXTURE1);gl.bindTexture(gl.TEXTURE_3D,lutTex);params(gl.TEXTURE_3D);gl.texImage3D(gl.TEXTURE_3D,0,gl.RGBA8,table.size,table.size,table.size,0,gl.RGBA,gl.UNSIGNED_BYTE,new Uint8Array(table.rgba));
      gl.uniform1f(gl.getUniformLocation(program,'size'),table.size);gl.drawArrays(gl.TRIANGLE_STRIP,0,4);
      if(gl.getError()!==gl.NO_ERROR)return;
      canvas.hidden=false;badge.hidden=false;
    } catch { hide(); } finally { running=false;if(latest)run(); }
  }
  canvas.addEventListener('webglcontextlost', e => { e.preventDefault(); clear(); dead=true;release(); });
  return {
    request(id, ops) { clear(); if (!allowed() || dead) return; const info=getInfo();if(!info||info.id!==id)return; const token=serial;latest={id,ops:structuredClone(ops),token,url:info.proxy_url||info.orig_url};timer=setTimeout(run,30); },
    clear,
    reset() { clear();source=null;sourcePromise=null;sourceUrl='';release();dead=false; },
  };
}
