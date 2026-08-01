const SOUNDS = {
  shoot: 'sounds/shoot.mp3',
  shipdeath: 'sounds/shipdeath.mp3',
  move: 'sounds/move.mp3',
  boost: 'sounds/boost.mp3',
  impact: 'sounds/impact.mp3',
  rockbreak: 'sounds/rockbreak.mp3',
  hitmarker_2: 'sounds/hitmarker_2.mp3',
  flare_deploy: 'sounds/flare_deploy.mp3',
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
  flare_deploy: 0.55,
  music: 1.0,
};
const PLAY_THROTTLE = {
  shoot: 0.03,
};
// Cockpit voice warnings. Kept separate from SOUNDS because they go through
// warn() rather than play() — they arbitrate against each other instead of
// stacking, the way a real voice warning system does.
const WARNINGS = {
  pull_up: 'sounds/warnings/pull_up.mp3',
  altitude: 'sounds/warnings/altitude.mp3',
  caution: 'sounds/warnings/caution.mp3',
  warning: 'sounds/warnings/warning.mp3',
  master_caution: 'sounds/warnings/master_caution.mp3',
  lock: 'sounds/warnings/lock.mp3',
  rwr_lock: 'sounds/warnings/rwr_lock.mp3',
  bingo: 'sounds/warnings/bingo.mp3',
  flare: 'sounds/warnings/flare.mp3',
  jammer: 'sounds/warnings/jammer.mp3',
  tws_search: 'sounds/warnings/tws_search.mp3',
  tws_lock: 'sounds/warnings/tws_lock.mp3',
  tws_launch_1: 'sounds/warnings/tws_launch_1.mp3',
  tws_launch_2: 'sounds/warnings/tws_launch_2.mp3',
};
// Higher wins. A warning already speaking is only cut off by something
// strictly more urgent — "pull up" interrupts "bingo" mid-word, never the
// reverse. Without this the callouts talk over each other and become noise.
const WARN_PRIORITY = {
  pull_up: 100,
  warning: 80,
  lock: 70, rwr_lock: 70, tws_launch_1: 70, tws_launch_2: 70,
  altitude: 55,
  caution: 50, tws_lock: 50,
  master_caution: 40,
  jammer: 35, tws_search: 30,
  bingo: 20, flare: 15,
};
// Minimum seconds between repeats of the same callout. The classic failure
// is "PULL UP" firing forty times in one canyon run until players mute.
const WARN_COOLDOWN = {
  pull_up: 1.6, altitude: 3.0, caution: 4.0,
  warning: 6.0, master_caution: 4.0,
  lock: 3.0, rwr_lock: 3.0,
  bingo: 12.0, flare: 0.6, jammer: 5.0,
  tws_search: 4.0, tws_lock: 4.0, tws_launch_1: 3.0, tws_launch_2: 3.0,
};
const WARN_VOLUME = 0.7;
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
  for (const [name, url] of Object.entries({ ...SOUNDS, ...WARNINGS })) {
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
  // --- voice warning system -------------------------------------------------
  // One voice at a time, arbitrated by priority, with a per-callout cooldown.
  let activeWarn = null; // { name, priority, src, gain }
  const lastWarned = {};
  function warn(name) {
    const buf = buffers[name];
    if (!buf) return false;
    const now = ctx.currentTime;
    const cooldown = WARN_COOLDOWN[name] ?? 2.0;
    if ((now - (lastWarned[name] || -Infinity)) < cooldown) return false;
    const priority = WARN_PRIORITY[name] ?? 0;
    if (activeWarn) {
      // Something is already speaking. Only a strictly more urgent callout
      // cuts it off; equal priority waits its turn rather than doubling up.
      if (priority <= activeWarn.priority) return false;
      try { activeWarn.src.stop(); } catch { /* already ended */ }
      activeWarn = null;
    }
    const src = ctx.createBufferSource();
    const gain = ctx.createGain();
    src.buffer = buf;
    gain.gain.value = WARN_VOLUME;
    src.connect(gain).connect(sfxMaster);
    const entry = { name, priority, src, gain };
    src.onended = () => { if (activeWarn === entry) activeWarn = null; };
    src.start(0);
    activeWarn = entry;
    lastWarned[name] = now;
    return true;
  }
  // Lets an EMP silence the cockpit mid-sentence.
  function stopWarnings() {
    if (!activeWarn) return;
    try { activeWarn.src.stop(); } catch { /* already ended */ }
    activeWarn = null;
  }

  const loops = {};
  function startSeamlessLoop(buf, { isMusic = false } = {}) {
    const masterGain = ctx.createGain();
    masterGain.gain.value = 0;
    masterGain.connect(isMusic ? ctx.destination : sfxMaster);
    const dur = buf.duration;
    const xfade = Math.min(0.08, dur * 0.1);
    const cycle = dur - xfade;
    const state = { gain: masterGain };
    let nextStart = ctx.currentTime + 0.05;
    function scheduleCycle(startTime) {
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
  return { play, warn, stopWarnings, setLoopVolume, setMusicVolume, setSfxVolume };
}
