import * as THREE from 'three';

// Third-person chase camera. Follows behind the ship in its local frame so
// rolls and pitches read clearly. RMB-drag enters a free-look mode that
// orbits around the ship without affecting flight.
export class ThirdPersonCamera {
  constructor(camera, target) {
    this.camera = camera;
    this.target = target;

    this.distance = 11;
    this.heightOffset = 5.6;
    this.lookAhead = 4;

    // Free-look orbit angles (only active while RMB held).
    this.yaw = 0;
    this.pitch = 0.2;
    this.sensitivity = 0.0035;
    this.minPitch = -Math.PI / 2 + 0.1;
    this.maxPitch = Math.PI / 2 - 0.1;

    this._desired = new THREE.Vector3();
    this._upDesired = new THREE.Vector3(0, 1, 0);
  }

  snap() {
    const back = new THREE.Vector3(0, 0, -1).applyQuaternion(this.target.quaternion);
    const up = new THREE.Vector3(0, 1, 0).applyQuaternion(this.target.quaternion);
    this._desired.copy(this.target.position)
      .addScaledVector(back, this.distance)
      .addScaledVector(up, this.heightOffset);
    this.camera.position.copy(this._desired);
    this._upDesired.copy(up);
    this.camera.up.copy(up);
    this.yaw = 0;
    this.pitch = 0.2;
  }

  update(dt, input) {
    const { dx, dy } = input.consumeMouseDelta();

    if (input.rmb) {
      this.yaw -= dx * this.sensitivity;
      this.pitch -= dy * this.sensitivity;
      this.pitch = Math.max(this.minPitch, Math.min(this.maxPitch, this.pitch));
      this._updateFreeLook(dt);
    } else {
      // Decay free-look angles back to neutral so re-entering RMB feels
      // continuous instead of snapping back to zero.
      this.yaw *= Math.pow(0.001, dt);
      this.pitch = THREE.MathUtils.damp(this.pitch, 0.2, 4, dt);
      this._updateChase(dt);
    }
  }

  _updateChase(dt) {
    // Position camera behind+above the ship in its local frame so banking
    // and pitching carry through.
    const back = new THREE.Vector3(0, 0, -1).applyQuaternion(this.target.quaternion);
    const up = new THREE.Vector3(0, 1, 0).applyQuaternion(this.target.quaternion);
    const fwd = new THREE.Vector3(0, 0, 1).applyQuaternion(this.target.quaternion);

    this._desired.copy(this.target.position)
      .addScaledVector(back, this.distance)
      .addScaledVector(up, this.heightOffset);

    const lerpAmt = 1 - Math.pow(0.0001, dt);
    this.camera.position.lerp(this._desired, lerpAmt);

    this._upDesired.lerp(up, 1 - Math.pow(0.01, dt));
    this.camera.up.copy(this._upDesired);

    // Blend the view direction between "ship-forward" (ship low on screen,
    // bullets read horizontal) and "look at ship" (ship centered). Small
    // blend tilts the view down just enough to lift the ship higher in the
    // frame without losing the parallel-bullet feel.
    const dirToShip = this.target.position.clone().sub(this.camera.position).normalize();
    const viewDir = fwd.clone().lerp(dirToShip, 0.25).normalize();
    const lookAt = this.camera.position.clone().addScaledVector(viewDir, 100);
    this.camera.lookAt(lookAt);
  }

  _updateFreeLook(dt) {
    const cosP = Math.cos(this.pitch);
    const offset = new THREE.Vector3(
      Math.sin(this.yaw) * cosP,
      Math.sin(this.pitch),
      Math.cos(this.yaw) * cosP
    ).multiplyScalar(this.distance);
    this._desired.copy(this.target.position).add(offset);

    const lerpAmt = 1 - Math.pow(0.001, dt);
    this.camera.position.lerp(this._desired, lerpAmt);

    this._upDesired.lerp(new THREE.Vector3(0, 1, 0), 1 - Math.pow(0.01, dt));
    this.camera.up.copy(this._upDesired);
    this.camera.lookAt(this.target.position);
  }
}
