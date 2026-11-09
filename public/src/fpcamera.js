import * as THREE from 'three';

// First-person cockpit camera.
//
// Mirrors ThirdPersonCamera's surface (snap / update) so it drops into the same call sites.
// The optional third `update` argument carries flight telemetry; ThirdPersonCamera ignores it.
//
// Head model: the ship's heading is NEVER driven by head movement.
//   - steering input leans the head into the turn (subtle, automatic)
//   - hold RMB for clamped free-look, which damps back to boresight on release
//   - hold Alt to look back over the shoulder
//
// Orientation note: a THREE camera looks down its local -Z, but this game's ship forward is
// +Z (main.js uses (0,0,1).applyQuaternion(ship.quaternion) everywhere). Hence the PI yaw
// correction baked into _apply().

const _eye = new THREE.Vector3();
const _qYaw = new THREE.Quaternion();
const _qPitch = new THREE.Quaternion();
const _qHead = new THREE.Quaternion();
const Y_AXIS = new THREE.Vector3(0, 1, 0);
const X_AXIS = new THREE.Vector3(1, 0, 0);

const DEG = Math.PI / 180;

export class FirstPersonCamera {
  constructor(camera, target, profile) {
    this.camera = camera;
    this.target = target;

    this.yaw = 0;
    this.pitch = 0;
    this.leanYaw = 0;
    this.leanPitch = 0;

    this.sensitivity = 0.0026;
    this.maxYaw = 110 * DEG;
    this.maxPitch = 70 * DEG;
    this.lookBackYaw = 180 * DEG;

    // How far the head drifts into a turn at full steer deflection.
    this.autoLeanYaw = 12 * DEG;
    this.autoLeanPitch = 7 * DEG;

    this.shakeT = 0;
    this.shakeAmp = 0;

    this.setProfile(profile);
  }

  setProfile(profile) {
    this.eyeLocal = profile.eye.clone();
    this.fov = profile.fov ?? 82;
  }

  snap() {
    this.yaw = 0;
    this.pitch = 0;
    this.leanYaw = 0;
    this.leanPitch = 0;
    this.shakeAmp = 0;
    this._apply();
  }

  update(dt, input, tel = null) {
    // Always drain the delta so it cannot accumulate while free-look is inactive.
    const { dx, dy } = input.consumeMouseDelta();

    const lookBack = input.keys.has('AltLeft') || input.keys.has('AltRight');
    const freeLook = !lookBack && (input.rmb || input.gp.freeLook);

    if (lookBack) {
      this.yaw = THREE.MathUtils.damp(this.yaw, this.lookBackYaw, 9, dt);
      this.pitch = THREE.MathUtils.damp(this.pitch, 0, 9, dt);
    } else if (freeLook) {
      if (input.rmb) {
        this.yaw -= dx * this.sensitivity;
        this.pitch -= dy * this.sensitivity;
      }
      if (input.gp.freeLook) {
        this.yaw -= input.gp.steerX * 2.2 * dt;
        this.pitch -= input.gp.steerY * 1.8 * dt;
      }
      this.yaw = clamp(this.yaw, -this.maxYaw, this.maxYaw);
      this.pitch = clamp(this.pitch, -this.maxPitch, this.maxPitch);
    } else {
      // Damped return to boresight.
      this.yaw = THREE.MathUtils.damp(this.yaw, 0, 7, dt);
      this.pitch = THREE.MathUtils.damp(this.pitch, 0, 7, dt);
    }

    // Automatic lean into the turn, from the ship's effective steer input.
    const sx = tel ? (tel.steerX ?? 0) : 0;
    const sy = tel ? (tel.steerY ?? 0) : 0;
    this.leanYaw = THREE.MathUtils.damp(this.leanYaw, -sx * this.autoLeanYaw, 5, dt);
    this.leanPitch = THREE.MathUtils.damp(this.leanPitch, -sy * this.autoLeanPitch, 5, dt);

    // Airframe shake: a constant rumble under boost, plus a decaying kick on damage.
    // tel.hitFlash reuses main.js's existing damage vignette envelope (1 -> 0).
    this.shakeT += dt;
    const boostRumble = tel?.boosting ? 0.0030 : 0;
    const hitKick = (tel?.hitFlash ?? 0) * 0.014;
    this.shakeAmp = boostRumble + hitKick;

    this._apply();
  }

  _apply() {
    const t = this.target;
    t.updateMatrixWorld();

    // Eye anchor is ship-local; localToWorld carries SHIP_SCALE for us.
    _eye.copy(this.eyeLocal);
    t.localToWorld(_eye);
    this.camera.position.copy(_eye);

    // Two incommensurable frequencies per axis so the rumble never reads as a loop.
    const st = this.shakeT;
    const shakeYaw = this.shakeAmp * (Math.sin(st * 47.1) + 0.6 * Math.sin(st * 113.7));
    const shakePitch = this.shakeAmp * (Math.sin(st * 61.3) + 0.6 * Math.sin(st * 149.2));

    // ship orientation * PI yaw (forward-axis fix) * head yaw * head pitch
    _qYaw.setFromAxisAngle(Y_AXIS, Math.PI + this.yaw + this.leanYaw + shakeYaw);
    _qPitch.setFromAxisAngle(X_AXIS, this.pitch + this.leanPitch + shakePitch);
    _qHead.copy(_qYaw).multiply(_qPitch);

    this.camera.quaternion.copy(t.quaternion).multiply(_qHead);
    // Setting the quaternion directly keeps roll correct; camera.up must not fight it.
    this.camera.up.set(0, 1, 0).applyQuaternion(this.camera.quaternion);

    // Re-assert FOV every frame: warp.js captures a baseFov at construction and restores it
    // when the intro warp ends, which would otherwise stomp the cockpit FOV.
    if (this.camera.fov !== this.fov) {
      this.camera.fov = this.fov;
      this.camera.updateProjectionMatrix();
    }
  }
}

function clamp(v, lo, hi) {
  return v < lo ? lo : v > hi ? hi : v;
}
