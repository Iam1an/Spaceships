import * as THREE from 'three';

const MISSILE_SPEED = 280;  // units/second
const TURN_RATE = 1.8;      // radians/second max angular velocity
const LIFE = 8.0;           // seconds before self-destruct
const HIT_RADIUS = 6.0;     // hit detection sphere radius

export function createMissiles() {
  const group = new THREE.Group();
  group.name = 'Missiles';

  const BODY_LEN = 5;
  const bodyGeo = new THREE.CylinderGeometry(0.3, 0.6, BODY_LEN, 8, 1);
  bodyGeo.rotateX(Math.PI / 2); // point along +Z
  const thrusterGeo = new THREE.SphereGeometry(1.2, 8, 5);

  const bodyMat = new THREE.MeshBasicMaterial({
    color: 0xff4400,
    transparent: true, opacity: 0.92,
    blending: THREE.AdditiveBlending, depthWrite: false,
  });
  const thrusterMat = new THREE.MeshBasicMaterial({
    color: 0xffaa33,
    transparent: true, opacity: 0.75,
    blending: THREE.AdditiveBlending, depthWrite: false,
  });

  const explosionGeo = new THREE.SphereGeometry(1, 10, 7);

  const missiles = [];
  const explosions = [];
  const _fwd = new THREE.Vector3(0, 0, 1);
  const _toTarget = new THREE.Vector3();
  const _currDir = new THREE.Vector3();
  const _newDir = new THREE.Vector3();

  function fire(origin, direction, targetRecord) {
    const normDir = direction.clone().normalize();
    const mesh = new THREE.Group();
    mesh.add(new THREE.Mesh(bodyGeo, bodyMat));
    const thruster = new THREE.Mesh(thrusterGeo, thrusterMat);
    thruster.position.z = -(BODY_LEN / 2 + 0.5);
    mesh.add(thruster);
    mesh.position.copy(origin);
    mesh.quaternion.setFromUnitVectors(_fwd, normDir);
    group.add(mesh);
    missiles.push({ mesh, vel: normDir.multiplyScalar(MISSILE_SPEED), target: targetRecord, life: LIFE });
  }

  function spawnExplosion(pos) {
    const mat = new THREE.MeshBasicMaterial({
      color: 0xff6611, transparent: true, opacity: 0.95,
      blending: THREE.AdditiveBlending, depthWrite: false,
    });
    const mesh = new THREE.Mesh(explosionGeo, mat);
    mesh.position.copy(pos);
    mesh.scale.setScalar(2);
    group.add(mesh);
    explosions.push({ mesh, age: 0, life: 0.7, from: 2, to: 14 });
  }

  function update(dt, remoteShips, onHitRemote, shooterTeam) {
    for (let i = missiles.length - 1; i >= 0; i--) {
      const m = missiles[i];
      m.life -= dt;
      let consumed = m.life <= 0;

      if (!consumed) {
        // Homing: steer toward target
        const t = m.target;
        if (t && t.alive && t.ship) {
          _toTarget.copy(t.ship.position).sub(m.mesh.position);
          const dist = _toTarget.length();
          if (dist > 0.5) {
            _toTarget.divideScalar(dist); // normalize in-place
            _currDir.set(0, 0, 1).applyQuaternion(m.mesh.quaternion);
            const dot = Math.max(-1, Math.min(1, _currDir.dot(_toTarget)));
            const angleDiff = Math.acos(dot);
            if (angleDiff > 0.001) {
              const factor = Math.min(1, (TURN_RATE * dt) / angleDiff);
              _newDir.lerpVectors(_currDir, _toTarget, factor).normalize();
              m.mesh.quaternion.setFromUnitVectors(_fwd, _newDir);
              m.vel.copy(_newDir).multiplyScalar(MISSILE_SPEED);
            }
          }
        }

        m.mesh.position.addScaledVector(m.vel, dt);

        // Hit detection against remote ships
        if (remoteShips) {
          for (const [id, r] of remoteShips) {
            if (!r.alive) continue;
            if (shooterTeam !== undefined && shooterTeam !== null && r.team === shooterTeam) continue;
            const dx = m.mesh.position.x - r.ship.position.x;
            const dy = m.mesh.position.y - r.ship.position.y;
            const dz = m.mesh.position.z - r.ship.position.z;
            if (dx * dx + dy * dy + dz * dz < HIT_RADIUS * HIT_RADIUS) {
              spawnExplosion(m.mesh.position.clone());
              if (onHitRemote) onHitRemote(id);
              consumed = true;
              break;
            }
          }
        }
      } else {
        // Lifetime expired: puff of smoke
        spawnExplosion(m.mesh.position.clone());
      }

      if (consumed) {
        group.remove(m.mesh);
        missiles.splice(i, 1);
      }
    }

    // Animate explosions
    for (let i = explosions.length - 1; i >= 0; i--) {
      const e = explosions[i];
      e.age += dt;
      const t = e.age / e.life;
      e.mesh.scale.setScalar(THREE.MathUtils.lerp(e.from, e.to, t));
      e.mesh.material.opacity = (1 - t) * 0.95;
      if (t >= 1) {
        group.remove(e.mesh);
        e.mesh.material.dispose();
        explosions.splice(i, 1);
      }
    }
  }

  return { group, fire, update };
}
