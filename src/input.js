// Tracks held keys, mouse position (normalized from screen center),
// scroll wheel deltas, and right-mouse drag for free-look.
//
// Steering input is the cursor's offset from screen center, normalized
// against half the viewport height so vertical and horizontal ranges
// match. While RMB is held, steering is suppressed and mouse movement
// instead drives the camera (consumed by ThirdPersonCamera).
//
// Touch mode: the same steerX/Y are driven by a virtual joystick (first
// touch on the left half anchors the stick base; subsequent drag fills
// the deflection). A second touch anywhere is ignored by the steering
// layer so the player can rest the right thumb on a button without
// hijacking the camera. On-screen buttons + slider elsewhere in the
// codebase write into `keys`/`lmb` directly to map Shift/Space/etc.
export class Input {
  constructor(domElement) {
    this.keys = new Set();
    this.lmb = false;
    this.rmb = false;
    // When true, mouse events are ignored entirely (steering, fire, look,
    // wheel). Used by the keyboard-only control scheme.
    this.mouseDisabled = false;
    // When true, touch events drive steering + free-look. Independent of
    // mouseDisabled — phones get both on for hybrid laptops, etc.
    this.touchEnabled = false;

    // Steering: cursor offset from center, clamped to [-1, 1].
    this.steerX = 0;
    this.steerY = 0;
    // Virtual cursor in pixels from screen center. Used when pointer lock
    // is active (real cursor doesn't exist) — we accumulate movementX/Y
    // and clamp to a half-screen range. Mirrors real cursor position when
    // unlocked, so toggling between modes is seamless.
    this.virtualX = 0;
    this.virtualY = 0;

    // Throttle delta from wheel (-1 ... +1 normalized chunks).
    this.wheel = 0;
    // Mobile throttle slider: 0..1 (fraction of MAX_THROTTLE) when the
    // player has touched the slider, null otherwise. Non-null = main.js
    // overrides W/S/wheel keys and pins targetThrottle to this value.
    this.throttleOverride = null;

    // RMB look deltas (consumed by camera).
    this.dx = 0;
    this.dy = 0;

    // Touch joystick state. Anchored at first touchstart, deflection is
    // clamped to ±stickRadius px. The overlay reads these to render the
    // stick base + knob.
    this.stickActive = false;
    this.stickBaseX = 0;
    this.stickBaseY = 0;
    this.stickKnobX = 0;
    this.stickKnobY = 0;
    this._stickId = null;
    this._stickRadius = 80;   // px → full deflection

    // Gamepad (Web Gamepad API). Polled each frame via pollGamepad().
    // gp.fire/drift/boost are hold-type flags; missile/flare/gunToggle
    // inject into this.keys for a single frame (edge-detected by main.js).
    this.gpConnected = false;
    this.gp = {
      steerX: 0,       // right stick X → yaw
      steerY: 0,       // right stick Y → pitch
      rollAxis: 0,     // left stick X → roll (-1 left, +1 right)
      throttleAxis: 0, // left stick Y, negated (+1 forward, -1 back)
      fire: false,     // RT or A
      drift: false,    // LT
      boost: false,    // LB
    };
    this._gpPrevMissile = false;
    this._gpPrevFlare = false;
    this._gpPrevGunToggle = false;

    window.addEventListener('keydown', (e) => {
      this.keys.add(e.code);
      // Suppress browser default for keys we use as gameplay input.
      if (e.code === 'Space'
        || e.code === 'ArrowUp' || e.code === 'ArrowDown'
        || e.code === 'ArrowLeft' || e.code === 'ArrowRight') {
        e.preventDefault();
      }
    });
    window.addEventListener('keyup', (e) => this.keys.delete(e.code));
    window.addEventListener('blur', () => this.keys.clear());

    domElement.addEventListener('contextmenu', (e) => e.preventDefault());

    domElement.addEventListener('mousedown', (e) => {
      if (this.mouseDisabled) return;
      if (e.button === 0) this.lmb = true;
      if (e.button === 2) this.rmb = true;
    });
    window.addEventListener('mouseup', (e) => {
      if (this.mouseDisabled) return;
      if (e.button === 0) this.lmb = false;
      if (e.button === 2) this.rmb = false;
    });

    window.addEventListener('mousemove', (e) => {
      if (this.mouseDisabled) return;
      if (this.rmb) {
        this.dx += e.movementX;
        this.dy += e.movementY;
        return;
      }
      const halfH = window.innerHeight * 0.5;
      const halfW = window.innerWidth * 0.5;
      if (document.pointerLockElement) {
        // Locked: there's no real cursor, so accumulate deltas and clamp.
        this.virtualX = Math.max(-halfH, Math.min(halfH, this.virtualX + e.movementX));
        this.virtualY = Math.max(-halfH, Math.min(halfH, this.virtualY + e.movementY));
        this.steerX = Math.max(-1, Math.min(1, this.virtualX / halfH));
        this.steerY = Math.max(-1, Math.min(1, this.virtualY / halfH));
      } else {
        const nx = (e.clientX - halfW) / halfH;
        const ny = (e.clientY - halfH) / halfH;
        this.steerX = Math.max(-1, Math.min(1, nx));
        this.steerY = Math.max(-1, Math.min(1, ny));
        this.virtualX = nx * halfH;
        this.virtualY = ny * halfH;
      }
    });

    // Wheel: positive deltaY = scroll down = throttle down.
    window.addEventListener('wheel', (e) => {
      if (this.mouseDisabled) return;
      this.wheel += -Math.sign(e.deltaY);
      e.preventDefault();
    }, { passive: false });

    // ---- Touch ----------------------------------------------------------
    // Touches whose target is a [data-touch-control] (HUD button) are
    // skipped here so the button's own handler runs. Everything else is
    // treated as steering (left half) or free-look (right half).
    const isControlTouch = (t) => !!t.target?.closest?.('[data-touch-control]');

    const onTouchStart = (e) => {
      if (!this.touchEnabled) return;
      let claimed = false;
      for (const t of e.changedTouches) {
        if (isControlTouch(t)) continue;
        // Only the FIRST non-control touch claims the steering stick;
        // subsequent touches are ignored so a second thumb resting on
        // the canvas doesn't yank steering or trigger free-look.
        if (this._stickId !== null) continue;
        if (t.clientX >= window.innerWidth * 0.5) continue;
        this._stickId = t.identifier;
        this.stickActive = true;
        this.stickBaseX = t.clientX;
        this.stickBaseY = t.clientY;
        this.stickKnobX = t.clientX;
        this.stickKnobY = t.clientY;
        this.steerX = 0;
        this.steerY = 0;
        claimed = true;
      }
      if (claimed) e.preventDefault();
    };

    const onTouchMove = (e) => {
      if (!this.touchEnabled) return;
      let claimed = false;
      for (const t of e.changedTouches) {
        if (t.identifier !== this._stickId) continue;
        const dx = t.clientX - this.stickBaseX;
        const dy = t.clientY - this.stickBaseY;
        const r = this._stickRadius;
        const len = Math.hypot(dx, dy);
        // Clamp the knob to the stick radius (visual + steering).
        const k = len > r ? r / len : 1;
        const kx = dx * k, ky = dy * k;
        this.stickKnobX = this.stickBaseX + kx;
        this.stickKnobY = this.stickBaseY + ky;
        this.steerX = kx / r;
        this.steerY = ky / r;
        claimed = true;
      }
      if (claimed) e.preventDefault();
    };

    const onTouchEnd = (e) => {
      if (!this.touchEnabled) return;
      for (const t of e.changedTouches) {
        if (t.identifier === this._stickId) {
          this._stickId = null;
          this.stickActive = false;
          this.steerX = 0;
          this.steerY = 0;
        }
      }
    };

    // Listen on window so a touch that drifts off the canvas still routes.
    window.addEventListener('touchstart', onTouchStart, { passive: false });
    window.addEventListener('touchmove', onTouchMove, { passive: false });
    window.addEventListener('touchend', onTouchEnd);
    window.addEventListener('touchcancel', onTouchEnd);
  }

  consumeMouseDelta() {
    const dx = this.dx, dy = this.dy;
    this.dx = 0;
    this.dy = 0;
    return { dx, dy };
  }

  consumeWheel() {
    const w = this.wheel;
    this.wheel = 0;
    return w;
  }

  // Called once per frame from the game loop. Reads the first connected
  // gamepad and maps it onto the same input state the keyboard/mouse use.
  //
  // Layout (Standard Gamepad — Xbox/PlayStation compatible):
  //   Axes:    0=LS-X  1=LS-Y  2=RS-X  3=RS-Y
  //   Buttons: 0=A/Cross  1=B/Circle  2=X/Square  3=Y/Triangle
  //            4=LB/L1    5=RB/R1     6=LT/L2     7=RT/R2
  //            8=Select   9=Start    10=L3        11=R3
  //           12=D↑      13=D↓      14=D←        15=D→
  pollGamepad() {
    const DEAD = 0.12;
    const dead = (v) => {
      const a = Math.abs(v);
      return a < DEAD ? 0 : Math.sign(v) * (a - DEAD) / (1 - DEAD);
    };

    const gamepad = [...(navigator.getGamepads?.() ?? [])].find(g => g?.connected);
    this.gpConnected = !!gamepad;

    if (!gamepad) {
      this.gp.steerX = 0;
      this.gp.steerY = 0;
      this.gp.rollAxis = 0;
      this.gp.throttleAxis = 0;
      this.gp.fire = false;
      this.gp.drift = false;
      this.gp.boost = false;
      return;
    }

    const ax = gamepad.axes;
    const bt = gamepad.buttons;
    const btn = (i) => bt[i]?.pressed ?? false;
    const val = (i) => bt[i]?.value ?? (btn(i) ? 1 : 0);

    // Right stick → steer (yaw/pitch). Only overrides mouse when stick moves.
    this.gp.steerX = dead(ax[2] ?? 0);
    this.gp.steerY = dead(ax[3] ?? 0);

    // Left stick X → roll; left stick Y (negated) → throttle
    this.gp.rollAxis = dead(ax[0] ?? 0);
    this.gp.throttleAxis = -dead(ax[1] ?? 0);  // stick up (−) = throttle up

    // Hold-type actions
    this.gp.fire  = val(7) > 0.5 || btn(0);   // RT or A
    this.gp.drift = val(6) > 0.5;             // LT
    this.gp.boost = btn(4);                   // LB

    // Edge-detected actions: inject into this.keys like a keydown/keyup pair
    const gpMissile   = btn(5) || btn(2);     // RB or X
    const gpFlare     = btn(1);               // B
    const gpGunToggle = btn(3);               // Y

    if (gpMissile   && !this._gpPrevMissile)   this.keys.add('KeyE');
    if (!gpMissile  &&  this._gpPrevMissile)   this.keys.delete('KeyE');
    if (gpFlare     && !this._gpPrevFlare)     this.keys.add('KeyQ');
    if (!gpFlare    &&  this._gpPrevFlare)     this.keys.delete('KeyQ');
    if (gpGunToggle && !this._gpPrevGunToggle) this.keys.add('KeyP');
    if (!gpGunToggle && this._gpPrevGunToggle) this.keys.delete('KeyP');

    this._gpPrevMissile   = gpMissile;
    this._gpPrevFlare     = gpFlare;
    this._gpPrevGunToggle = gpGunToggle;
  }
}
