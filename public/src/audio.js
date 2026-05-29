// Web Audio API mixer. One-shots create a fresh BufferSource per play (cheap,
// can overlap freely). Loops use a single persistent BufferSource started
// once with `loop = true` and a GainNode for volume — looping happens at the
// sample level so there's no gap or restart glitch like HTMLAudioElement has.
const SOUNDS = {
  shoot:       './public/sounds/shoot.mp3',
  shipdeath:   './public/sounds/shipdeath.mp3',
  move:        './public/sounds/move.mp3',
  boost:       './public/sounds/boost.mp3',
  impact:      './public/sounds/impact.mp3',
  rockbreak:   './public/sounds/rockbreak.mp3',
  hitmarker_2: './public/sounds/hitmarker_2.mp3',
  // Background music. Auto-loops at MUSIC volume once decoded; if the
  // file is missing the loop simply never starts (silent fallback).
  music:       './public/sounds/dumb_Eflatmin.mp3',
};

const VOLUMES = {
  shoot: 0.28,
  shipdeath: 0.6,       
  move: 0.25,
  boost: 0.4,
  impact: 0.45,
  rockbreak: 0.55,
  hitmarker_2: 0.25,
  music: 1.0, // loop volume passed via setMusicVolume; this is the cap.
};

// Per-sound minimum interval between plays. Caps a noisy short sound
// (like the laser shoot) at a sane rate even when 8 bots dogpile: at
// 0.03s the global ceiling is ~33Hz, generous enough to hear every
// player shot plus overlapping bot volleys without flooring the CPU.
const PLAY_THROTTLE = {
  shoot: 0.03,
};

export function createAudio() {
  const Ctor = window.AudioContext || window.webkitAudioContext;
  if (!Ctor) return {
    play() {}, setLoopVolume() {}, setMusicVolume() {}, setSfxVolume() {},
  };
  const ctx = new Ctor();
  // Master SFX gain. Every non-music sound — one-shots and the engine
  // loops — routes through this. The music loop bypasses it so the
  // music slider is independent of the SFX slider.
  const sfxMaster = ctx.createGain();
  sfxMaster.gain.value = 1;
  sfxMaster.connect(ctx.destination);
  const lastPlayed = {};

  // Browsers may start the context suspended until user gesture. Resume on
  // first click/keypress.
  if (ctx.state === 'suspended') {
    const resume = () => {
      ctx.resume().catch(() => {});
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
      .catch(() => {});
  }

  function play(name, volMult = 1) {
    const buf = buffers[name];
    if (!buf) return;
    // Per-sound throttle (currently just 'shoot') so swarms of bots
    // can't drown everything else out.
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

  // Seamless loop via overlapping scheduled cycles. The naive approach
  // (BufferSource.loop = true) reveals any encoder padding or built-in
  // fade-in/out at the boundary, which sounds like a click or dropout
  // every cycle. Instead we schedule each play with its own short
  // linear crossfade so consecutive plays overlap and the listener
  // hears continuous audio regardless of how the file was encoded.
  function startSeamlessLoop(buf, { isMusic = false } = {}) {
    const masterGain = ctx.createGain();
    masterGain.gain.value = 0;
    masterGain.connect(isMusic ? ctx.destination : sfxMaster);
    const dur = buf.duration;
    // Crossfade tail: long enough to mask up to ~80ms of encoder
    // padding, short enough not to obviously double-up content.
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
      // Linear fade-in / fade-out at the cycle boundaries.
      fade.gain.setValueAtTime(0, startTime);
      fade.gain.linearRampToValueAtTime(1, startTime + xfade);
      fade.gain.setValueAtTime(1, startTime + dur - xfade);
      fade.gain.linearRampToValueAtTime(0, startTime + dur);
      src.start(startTime);
      src.stop(startTime + dur + 0.01);
      // When this cycle ends, chain another. We always keep ≥1 cycle
      // already scheduled ahead so onended latency can't cause a gap.
      src.onended = () => {
        if (!state.alive) return;
        scheduleCycle(nextStart);
        nextStart += cycle;
      };
    }
    // Prime the chain with two cycles so the first onended already has
    // a successor playing.
    scheduleCycle(nextStart); nextStart += cycle;
    scheduleCycle(nextStart); nextStart += cycle;
    return state;
  }

  function setLoopVolume(name, vol) {
    const buf = buffers[name];
    if (!buf) {
      // Buffer not loaded yet — remember the latest desired volume so we
      // honor it as soon as the decode finishes.
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
