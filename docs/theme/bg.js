(function () {
  var c = document.getElementById('bg-canvas');
  if (!c) return;
  var gl = c.getContext('webgl', { alpha: true, premultipliedAlpha: false, antialias: false })
        || c.getContext('experimental-webgl', { alpha: true, premultipliedAlpha: false, antialias: false });
  if (!gl) {
    c.style.background = 'transparent';
    return;
  }

  /*
   * Inspired by:
   *   - "A flowing WebGL gradient, deconstructed" - alexharri.com/blog/webgl-gradients
   *   - The Book of Shaders, ch. 11, 12, 13 (noise + fbm + domain warping)
   *   - "Tiny Clouds" - shadertoy.com (stubbe)
   *
   * Recipe: domain-warped fractional Brownian motion gates soft green stage
   * lights. The CSS hero adds column blur and grain; this shader supplies the
   * slow, high-resolution luminous movement underneath.
   */

  var vs = ''
    + 'attribute vec2 a;'
    + 'void main(){gl_Position=vec4(a,0.,1.);}';

  var fs = ''
    + 'precision highp float;'
    + 'uniform vec2 u_res;'
    + 'uniform float u_t;'
    + 'float hash(vec2 p){return fract(sin(dot(p,vec2(127.1,311.7)))*43758.5453);}'
    + 'float vnoise(vec2 p){'
    + ' vec2 i=floor(p),f=fract(p);'
    + ' vec2 u=f*f*(3.0-2.0*f);'
    + ' return mix(mix(hash(i),hash(i+vec2(1.0,0.0)),u.x),'
    + '            mix(hash(i+vec2(0.0,1.0)),hash(i+vec2(1.0,1.0)),u.x),u.y);'
    + '}'
    + 'float fbm(vec2 p){'
    + ' float v=0.0; float a=0.5;'
    + ' for(int i=0;i<5;i++){'
    + '  v+=a*vnoise(p);'
    + '  p=p*2.02+vec2(13.7,9.1);'
    + '  a*=0.5;'
    + ' }'
    + ' return v;'
    + '}'
    + 'void main(){'
    + ' vec2 uv=gl_FragCoord.xy/u_res;'
    /* Aspect-corrected coords scaled into noise space */
    + ' float aspect=u_res.x/u_res.y;'
    + ' vec2 p=vec2(uv.x*aspect, uv.y)*1.6;'
    + ' float t=u_t*0.06;'
    /* Two-stage domain warp - smooth swirling motion */
    + ' vec2 q=vec2(fbm(p+vec2(0.0,t)),'
    + '            fbm(p+vec2(5.2,1.3)+vec2(t,0.0)));'
    + ' vec2 r=vec2(fbm(p+3.0*q+vec2(1.7,9.2)+vec2(t*0.5,0.0)),'
    + '            fbm(p+3.0*q+vec2(8.3,2.8)+vec2(0.0,t*0.5)));'
    + ' float n=fbm(p+3.5*r);'
    /* Slow, large-scale mask - patches grow and fade in / out */
    + ' float mask=fbm(p*0.45+vec2(t*0.4,t*0.25));'
    + ' mask=smoothstep(0.32,0.78,mask);'
    + ' vec3 deep=vec3(0.012,0.030,0.020);'
    + ' vec3 moss=vec3(0.055,0.265,0.145);'
    + ' vec3 sage=vec3(0.44,0.56,0.46);'
    + ' vec3 milk=vec3(0.86,0.91,0.82);'
    + ' float beam=smoothstep(0.08,0.72,1.0-abs(uv.x-0.55)*1.65);'
    + ' float lift=smoothstep(0.04,0.82,uv.y);'
    + ' float glow=smoothstep(0.24,0.88,n*mask+beam*0.32+lift*0.18);'
    + ' vec3 col=mix(deep,moss,glow);'
    + ' col=mix(col,sage,smoothstep(0.40,0.92,fbm(p*0.7+vec2(2.0,-t))*beam));'
    + ' col=mix(col,milk,smoothstep(0.78,0.98,n)*0.20);'
    /* Stage-style top and side falloff keeps the light from feeling flat. */
    + ' float vig=smoothstep(0.40,1.05,distance(uv,vec2(0.50,0.48)))*0.42;'
    + ' col-=vig;'
    + ' col*=1.0-smoothstep(0.0,0.20,uv.x)*0.08;'
    + ' col=clamp(col,0.0,1.0);'
    + ' gl_FragColor=vec4(col,1.0);'
    + '}';

  function compile(type, src) {
    var sh = gl.createShader(type);
    gl.shaderSource(sh, src);
    gl.compileShader(sh);
    if (!gl.getShaderParameter(sh, gl.COMPILE_STATUS)) {
      console.error('shader error', gl.getShaderInfoLog(sh));
      return null;
    }
    return sh;
  }

  var v = compile(gl.VERTEX_SHADER, vs);
  var f = compile(gl.FRAGMENT_SHADER, fs);
  if (!v || !f) return;
  var prog = gl.createProgram();
  gl.attachShader(prog, v);
  gl.attachShader(prog, f);
  gl.linkProgram(prog);
  if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) return;
  gl.useProgram(prog);

  var buf = gl.createBuffer();
  gl.bindBuffer(gl.ARRAY_BUFFER, buf);
  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]), gl.STATIC_DRAW);
  var loc = gl.getAttribLocation(prog, 'a');
  gl.enableVertexAttribArray(loc);
  gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);

  var uRes = gl.getUniformLocation(prog, 'u_res');
  var uT = gl.getUniformLocation(prog, 'u_t');

  function resize() {
    var dpr = Math.min(window.devicePixelRatio || 1, 2);
    c.width = Math.floor(window.innerWidth * dpr);
    c.height = Math.floor(window.innerHeight * dpr);
    c.style.width = window.innerWidth + 'px';
    c.style.height = window.innerHeight + 'px';
    gl.viewport(0, 0, c.width, c.height);
  }
  window.addEventListener('resize', resize, { passive: true });
  resize();

  var prefersReduced = window.matchMedia
    && window.matchMedia('(prefers-reduced-motion: reduce)').matches;

  /*
   * Time origin is anchored in sessionStorage so navigation between pages
   * resumes the animation at the same frame instead of restarting at t=0.
   * The shader is deterministic for any given t, so the visible pattern
   * stays continuous from page to page.
   */
  var STORE_KEY = 'aionBgStart';
  var startMs;
  try {
    var stored = window.sessionStorage.getItem(STORE_KEY);
    if (stored && !isNaN(parseFloat(stored))) {
      startMs = parseFloat(stored);
    } else {
      startMs = Date.now();
      window.sessionStorage.setItem(STORE_KEY, String(startMs));
    }
  } catch (e) {
    startMs = Date.now();
  }

  var last = 0;
  var INTERVAL = 1000 / 30; /* throttle to 30fps */

  /* Render once synchronously so the first frame is correct before rAF kicks in */
  function render(t) {
    gl.uniform2f(uRes, c.width, c.height);
    gl.uniform1f(uT, t);
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
  }

  var initial = prefersReduced ? 0 : (Date.now() - startMs) / 1000;
  render(initial);

  function frame(now) {
    if (now - last >= INTERVAL) {
      last = now;
      var t = prefersReduced ? 0 : (Date.now() - startMs) / 1000;
      render(t);
    }
    requestAnimationFrame(frame);
  }
  requestAnimationFrame(frame);

  /* Pause work when the tab is hidden, resume on visibility */
  document.addEventListener('visibilitychange', function () {
    if (!document.hidden) last = 0;
  });

})();
