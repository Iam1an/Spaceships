// On-screen control overlay for the mobile scheme.
//
// Buttons write into the existing Input state: holding "BOOST" adds
// 'ShiftLeft' to input.keys, holding "FIRE" sets input.lmb, etc. — so the
// rest of the game reads input exactly like in keyboard mode.
//
// Mobile layout: stick visualization (driven by input.stickActive/Base/
// Knob) plus Fire/Boost/Drift buttons and a throttle pair on the right
// edge. Returns no-op stubs for any other scheme so callers don't branch.

// Tags any button DOM element so the touch handler in input.js knows to
// route touches on it to the button itself instead of the joystick.
const BUTTON_ATTR = 'data-touch-control';

export function createTouchHud({ input, scheme }) {
  if (scheme !== 'mobile') return { update() {}, destroy() {} };

  // All overlay elements are appended here and torn down together. CSS is
  // inline so we don't bloat index.html with mode-specific styles.
  const root = document.createElement('div');
  root.id = 'touchhud';
  root.style.cssText = [
    'position:fixed', 'inset:0',
    // Pointer events pass through the empty middle so the canvas can still
    // receive (touch-mode) joystick anchoring. Individual buttons re-enable
    // pointer-events on themselves.
    'pointer-events:none',
    'z-index:3',
    'user-select:none', '-webkit-user-select:none',
    '-webkit-touch-callout:none',
    'font-family:inherit',
  ].join(';');
  document.body.appendChild(root);

  // Stick visualizer. Updated each frame from input.stickActive/Base/Knob;
  // the touch handling itself is in input.js.
  const stickEl = document.createElement('div');
  stickEl.style.cssText = 'position:absolute;left:0;top:0;pointer-events:none;opacity:0;transition:opacity 0.12s ease;';
  const stickBaseEl = document.createElement('div');
  stickBaseEl.style.cssText = [
    'position:absolute', 'width:160px', 'height:160px',
    'margin:-80px 0 0 -80px',
    'border:3px solid rgba(176,224,255,0.55)',
    'border-radius:50%',
    'background:rgba(8,14,28,0.25)',
    'box-shadow:0 0 18px rgba(80,160,255,0.35)',
  ].join(';');
  const stickKnobEl = document.createElement('div');
  stickKnobEl.style.cssText = [
    'position:absolute', 'width:64px', 'height:64px',
    'margin:-32px 0 0 -32px',
    'border:3px solid #ffd97a',
    'border-radius:50%',
    'background:rgba(255,217,122,0.35)',
    'box-shadow:0 0 14px #ffd97a',
  ].join(';');
  stickEl.appendChild(stickBaseEl);
  stickEl.appendChild(stickKnobEl);
  root.appendChild(stickEl);

  // ---- Buttons --------------------------------------------------------
  // makeButton(label, opts) → DOM button that fires onHold(true)/onHold(false)
  // on press/release, including pointercancel and window-blur. We use
  // Pointer Events to unify mouse + touch + stylus on every button.
  const allButtons = [];
  function makeButton(label, opts) {
    const b = document.createElement('button');
    b.type = 'button';
    b.textContent = label;
    b.setAttribute(BUTTON_ATTR, '');
    b.style.cssText = [
      'position:absolute',
      'pointer-events:auto',
      'font-family:inherit',
      `font-size:${opts.fontSize || 11}px`,
      'letter-spacing:1px',
      'color:#fff',
      'border-radius:50%',
      `background:${opts.bg || 'rgba(8,14,28,0.7)'}`,
      `border:3px solid ${opts.border || '#b0e0ff'}`,
      `box-shadow:0 0 14px ${opts.glow || 'rgba(80,160,255,0.4)'}`,
      `width:${opts.size || 80}px`, `height:${opts.size || 80}px`,
      'text-shadow:1px 1px 0 #000',
      'cursor:pointer',
      'transition:transform 0.06s ease, opacity 0.1s ease',
      'touch-action:none',
      'opacity:0.85',
    ].join(';');
    Object.assign(b.style, opts.position || {});
    const press = (e) => {
      e.preventDefault();
      b.style.transform = 'scale(0.92)';
      b.style.opacity = '1';
      // Capture so a drag off the button still gets the release event.
      try { b.setPointerCapture?.(e.pointerId); } catch {}
      opts.onHold(true);
    };
    const release = (e) => {
      b.style.transform = '';
      b.style.opacity = '0.85';
      try { b.releasePointerCapture?.(e.pointerId); } catch {}
      opts.onHold(false);
    };
    b.addEventListener('pointerdown', press);
    b.addEventListener('pointerup', release);
    b.addEventListener('pointercancel', release);
    b.addEventListener('pointerleave', (e) => {
      // Only treat as release if the pointer was actually held.
      if (b.hasPointerCapture?.(e.pointerId)) release(e);
    });
    root.appendChild(b);
    allButtons.push({ el: b, release: () => opts.onHold(false) });
    return b;
  }

  // Helpers to hold/release a key code or the LMB flag.
  function holdKey(code) {
    return (down) => { down ? input.keys.add(code) : input.keys.delete(code); };
  }
  function holdLmb() {
    return (down) => { input.lmb = down; };
  }

  // Right-side cluster. FIRE is biggest and bottommost so a thumb resting
  // in the corner naturally lands on it; ROLL/BOOST/DRIFT stack above and
  // to its left. Throttle slider lives on the far-right edge above FIRE.
  makeButton('FIRE', {
    size: 120,
    bg: 'linear-gradient(180deg, #ff7070 0%, #c22a2a 100%)',
    border: '#ffb0b0', glow: 'rgba(255,80,80,0.6)',
    fontSize: 14,
    position: { right: '24px', bottom: '24px' },
    onHold: holdLmb(),
  });
  makeButton('DRIFT', {
    size: 72,
    bg: 'linear-gradient(180deg, #ffe07a 0%, #ff8833 100%)',
    border: '#ffd97a', glow: 'rgba(255,200,80,0.55)',
    position: { right: '168px', bottom: '24px' },
    onHold: holdKey('Space'),
  });
  makeButton('BOOST', {
    size: 72,
    bg: 'linear-gradient(180deg, #66ddff 0%, #2867d0 100%)',
    border: '#b0e0ff', glow: 'rgba(80,160,255,0.55)',
    position: { right: '168px', bottom: '108px' },
    onHold: holdKey('ShiftLeft'),
  });
  makeButton('⟲', {
    size: 64, fontSize: 28,
    position: { right: '240px', bottom: '192px' },
    onHold: holdKey('KeyA'),
  });
  makeButton('⟳', {
    size: 64, fontSize: 28,
    position: { right: '168px', bottom: '192px' },
    onHold: holdKey('KeyD'),
  });
  // MSL/FLARE map to the E/Q key-down edges main.js already listens for,
  // so a tap fires exactly one missile / one flare burst.
  makeButton('MSL', {
    size: 64,
    bg: 'linear-gradient(180deg, #ffaa66 0%, #cc4411 100%)',
    border: '#ffcc99', glow: 'rgba(255,140,60,0.55)',
    position: { right: '264px', bottom: '24px' },
    onHold: holdKey('KeyE'),
  });
  makeButton('FLARE', {
    size: 64, fontSize: 10,
    bg: 'linear-gradient(180deg, #fff0a0 0%, #d09020 100%)',
    border: '#ffe680', glow: 'rgba(255,220,100,0.55)',
    position: { right: '264px', bottom: '108px' },
    onHold: holdKey('KeyQ'),
  });

  // ---- Throttle slider ------------------------------------------------
  // Vertical track on the right edge above the FIRE button. Drag the
  // thumb to set targetThrottle absolutely (0 at bottom, MAX at top).
  // The slider is sticky — releasing keeps the throttle where you left
  // it, matching the wheel/W behaviour on desktop.
  const slider = document.createElement('div');
  slider.setAttribute(BUTTON_ATTR, '');
  slider.style.cssText = [
    'position:absolute', 'right:60px', 'bottom:160px',
    'width:40px', 'height:220px',
    'pointer-events:auto', 'touch-action:none',
    'background:rgba(8,14,28,0.7)',
    'border:2px solid #b0e0ff',
    'border-radius:22px',
    'box-shadow:0 0 10px rgba(80,160,255,0.35)',
  ].join(';');
  // Fill rises from the bottom so the colored area visualizes current %.
  const sliderFill = document.createElement('div');
  sliderFill.style.cssText = [
    'position:absolute', 'left:0', 'right:0', 'bottom:0',
    'height:0%',
    'background:linear-gradient(180deg, #66ddff 0%, #2867d0 100%)',
    'border-radius:20px', 'pointer-events:none',
  ].join(';');
  const sliderThumb = document.createElement('div');
  sliderThumb.style.cssText = [
    'position:absolute', 'left:50%', 'bottom:0',
    'width:48px', 'height:24px',
    'margin:0 0 -12px -24px',
    'background:#ffd97a',
    'border:2px solid #fff',
    'border-radius:12px',
    'box-shadow:0 0 12px #ffd97a',
    'pointer-events:none',
  ].join(';');
  const sliderLabel = document.createElement('div');
  sliderLabel.textContent = 'THR';
  sliderLabel.style.cssText = [
    'position:absolute', 'left:0', 'right:0', 'top:-20px',
    'text-align:center', 'font-size:11px',
    'color:#b0c8e0', 'letter-spacing:2px',
    'pointer-events:none',
  ].join(';');
  slider.appendChild(sliderFill);
  slider.appendChild(sliderThumb);
  slider.appendChild(sliderLabel);
  root.appendChild(slider);

  let throttleFrac = 0;
  function setThrottleFromTouchY(clientY) {
    const rect = slider.getBoundingClientRect();
    const y = clientY - rect.top;
    const f = Math.max(0, Math.min(1, 1 - y / rect.height));
    throttleFrac = f;
    input.throttleOverride = f;
    const pct = (f * 100).toFixed(0);
    sliderFill.style.height = pct + '%';
    sliderThumb.style.bottom = pct + '%';
  }
  let sliderPointer = null;
  slider.addEventListener('pointerdown', (e) => {
    e.preventDefault();
    sliderPointer = e.pointerId;
    try { slider.setPointerCapture(e.pointerId); } catch {}
    setThrottleFromTouchY(e.clientY);
  });
  slider.addEventListener('pointermove', (e) => {
    if (e.pointerId !== sliderPointer) return;
    setThrottleFromTouchY(e.clientY);
  });
  const releaseSlider = (e) => {
    if (e.pointerId !== sliderPointer) return;
    sliderPointer = null;
    try { slider.releasePointerCapture(e.pointerId); } catch {}
  };
  slider.addEventListener('pointerup', releaseSlider);
  slider.addEventListener('pointercancel', releaseSlider);

  // Safety: window-blur releases every held button (avoids stuck keys
  // when an alert / context switch steals focus mid-press).
  const onBlur = () => { for (const { release } of allButtons) release(); };
  window.addEventListener('blur', onBlur);

  function update() {
    if (!stickEl) return;
    if (input.stickActive) {
      stickEl.style.opacity = '1';
      stickBaseEl.style.left = input.stickBaseX + 'px';
      stickBaseEl.style.top = input.stickBaseY + 'px';
      stickKnobEl.style.left = input.stickKnobX + 'px';
      stickKnobEl.style.top = input.stickKnobY + 'px';
    } else {
      stickEl.style.opacity = '0';
    }
  }

  function destroy() {
    window.removeEventListener('blur', onBlur);
    root.remove();
  }

  return { update, destroy };
}
