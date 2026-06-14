const SOUNDS = {
  shoot: 'sounds/shoot.mp3',
  shipdeath: 'sounds/shipdeath.mp3',
  move: 'sounds/move.mp3',
  boost: 'sounds/boost.mp3',
  impact: 'sounds/impact.mp3',
  rockbreak: 'sounds/rockbreak.mp3',
  hitmarker_2: 'sounds/hitmarker_2.mp3',
  music: 'sounds/dumb_Eflatmin.mp3',
};
const VOLUMES = {
  shoot: 0.28,
  shipdeath: 0.6,
  move: 0.25,
  boost: 0.4,
  impact: 0.45,
  rockbreak: 0.55,
  hitmarker_2: 0.25,
  music: 1.0,
};
const PLAY_THROTTLE = {
  shoot: 0.03,
};
export function createAudio() {
  const Ctor = window.AudioContext || window.webkitAudioContext;
  if (!Ctor) return {
    play() { }, setLoopVolume() { }, setMusicVolume() { }, setSfxVolume() { },
  };
  const ctx = new Ctor();
  const sfxMaster = ctx.createGain();
  sfxMaster.gain.value = 1;
  sfxMaster.connect(ctx.destination);
  const lastPlayed = {};
  if (ctx.state === 'suspended') {
    const resume = () => {
      ctx.resume().catch(() => { });
      window.removeEventListener('pointerdown', resume);
      window.removeEventListener('keydown', resume);
    };
    window.addEventListener('pointerdown', resume);
    window.addEventListener('keydown', resume);
  }
  const buffers = {};
  const pendingLoops = {};
  for (const [name, url] of Object.entries(SOUNDS)) {
    fetch(url)
      .then((r) => r.arrayBuffer())
      .then((buf) => ctx.decodeAudioData(buf))
      .then((decoded) => {
        buffers[name] = decoded;
        if (pendingLoops[name] !== undefined) {
          const v = pendingLoops[name];
          delete pendingLoops[name];
          setLoopVolume(name, v);
        }
      })
      .catch(() => { });
  }
  function play(name, volMult = 1) {
    const buf = buffers[name];
    if (!buf) return;
    const throttle = PLAY_THROTTLE[name];
    if (throttle !== undefined) {
      const now = ctx.currentTime;
      if ((now - (lastPlayed[name] || 0)) < throttle) return;
      lastPlayed[name] = now;
    }
    const src = ctx.createBufferSource();
    src.buffer = buf;
    const gain = ctx.createGain();
    gain.gain.value = Math.max(0, Math.min(1, (VOLUMES[name] ?? 0.5) * volMult));
    src.connect(gain).connect(sfxMaster);
    src.start(0);
  }
  const loops = {};
  function startSeamlessLoop(buf, { isMusic = false } = {}) {
    const masterGain = ctx.createGain();
    masterGain.gain.value = 0;
    masterGain.connect(isMusic ? ctx.destination : sfxMaster);
    const dur = buf.duration;
    const xfade = Math.min(0.08, dur * 0.1);
    const cycle = dur - xfade;
    const state = { gain: masterGain, alive: true };
    let nextStart = ctx.currentTime + 0.05;
    function scheduleCycle(startTime) {
      if (!state.alive) return;
      const src = ctx.createBufferSource();
      const fade = ctx.createGain();
      src.buffer = buf;
      src.connect(fade).connect(masterGain);
      fade.gain.setValueAtTime(0, startTime);
      fade.gain.linearRampToValueAtTime(1, startTime + xfade);
      fade.gain.setValueAtTime(1, startTime + dur - xfade);
      fade.gain.linearRampToValueAtTime(0, startTime + dur);
      src.start(startTime);
      src.stop(startTime + dur + 0.01);
      src.onended = () => {
        if (!state.alive) return;
        scheduleCycle(nextStart);
        nextStart += cycle;
      };
    }
    scheduleCycle(nextStart); nextStart += cycle;
    scheduleCycle(nextStart); nextStart += cycle;
    return state;
  }
  function setLoopVolume(name, vol) {
    const buf = buffers[name];
    if (!buf) {
      pendingLoops[name] = vol;
      return;
    }
    let entry = loops[name];
    if (!entry) {
      entry = startSeamlessLoop(buf, { isMusic: name === 'music' });
      loops[name] = entry;
    }
    entry.gain.gain.value = Math.max(0, Math.min(1, vol));
  }
  function setMusicVolume(v) { setLoopVolume('music', v); }
  function setSfxVolume(v) {
    sfxMaster.gain.value = Math.max(0, Math.min(1, v));
  }
  return { play, setLoopVolume, setMusicVolume, setSfxVolume };
}
