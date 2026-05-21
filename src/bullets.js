import * as THREE from 'three';

export const BULLET_SPEED = 780;

// Projectile + impact-fx manager. Bullets are simple emissive spheres so
// they read against the dark skybox without needing a real light source.
// Hit detection is sphere/sphere against the asteroid list passed to update().
// `shipHitRadius` lets the caller bump the ship-hit sphere for forgiving
// aim modes (e.g. arrow-key / controller players who can't fine-aim).
export function createBullets({ shipHitRadius = 5.0 } = {}) {
  const group = new THREE.Group();
  group.name = 'Bullets';

  const SPEED = BULLET_SPEED;
  const LIFE = 2.0;
  const RADIUS = 0.5;

  // Laser bolt geometry: two concentric cylinders — a thin opaque core
  // for the bright streak + a wider transparent halo for the soft glow.
  // Default cylinder axis is +Y; we rotate the geometry so the axis is
  // +Z, then orient each bolt's Group via setFromUnitVectors so the bolt
  // visually points along its direction of travel.
  const LASER_LEN = 5.0;
  const CORE_GEO = new THREE.CylinderGeometry(0.09, 0.09, LASER_LEN, 8, 1, false);
  CORE_GEO.rotateX(Math.PI / 2);
  const HALO_GEO = new THREE.CylinderGeometry(0.32, 0.32, LASER_LEN * 1.15, 12, 1, false);
  HALO_GEO.rotateX(Math.PI / 2);
  // Per-faction palettes. Bright near-white core sells the "energy bolt"
  // feel; the halo color is what the player reads at distance.
  // Cyan-green for self, blue for allies, red for enemies.
  function mkLaserMats(haloColor, coreColor) {
    return {
      core: new THREE.MeshBasicMaterial({
        color: coreColor, transparent: true, opacity: 0.95,
        blending: THREE.AdditiveBlending, depthWrite: false,
      }),
      halo: new THREE.MeshBasicMaterial({
        color: haloColor, transparent: true, opacity: 0.55,
        blending: THREE.AdditiveBlending, depthWrite: false,
      }),
    };
  }
  const localMats = mkLaserMats(0x44ffb0, 0xeaffe6);
  const allyMats  = mkLaserMats(0x4aa3ff, 0xe6f0ff);
  const enemyMats = mkLaserMats(0xff3030, 0xffe0e0);

  const explosionGeo = new THREE.SphereGeometry(1, 14, 10);

  const bullets = [];
  const explosions = [];

  // Reused vectors so fire() doesn't allocate per shot.
  const _fwd = new THREE.Vector3(0, 0, 1);
  function fire(origin, direction, faction = 'self') {
    const mats = faction === 'self' ? localMats
              : faction === 'ally' ? allyMats
              : enemyMats;
    const isLocal = faction === 'self';
    const bolt = new THREE.Group();
    bolt.add(new THREE.Mesh(CORE_GEO, mats.core));
    bolt.add(new THREE.Mesh(HALO_GEO, mats.halo));
    bolt.position.copy(origin);
    // Align the bolt's local +Z (cylinder long axis after the rotateX
    // above) with the travel direction so the streak points the right way.
    bolt.quaternion.setFromUnitVectors(_fwd, direction);
    group.add(bolt);
    const vel = direction.clone().multiplyScalar(SPEED);
    bullets.push({ mesh: bolt, vel, life: LIFE, isLocal });
  }

  // Cap concurrent explosions so a swarm of bullets hitting one rock can't
  // tank low-end devices on fill-rate (additive transparent quads stack).
  // When at cap, recycle the oldest in place instead of allocating.
  const MAX_EXPLOSIONS = 12;
  function spawnExplosion(pos, scale) {
    if (explosions.length >= MAX_EXPLOSIONS) {
      // Replace oldest: reuse mesh + material, just reset timing/position.
      const e = explosions.shift();
      e.mesh.position.copy(pos);
      e.mesh.scale.setScalar(scale * 0.4);
      e.mesh.material.opacity = 0.95;
      e.age = 0;
      e.life = 0.55;
      e.from = scale * 0.4;
      e.to = scale * 2.6;
      explosions.push(e);
      return;
    }
    const mat = new THREE.MeshBasicMaterial({
      color: 0xffaa55,
      transparent: true,
      opacity: 0.95,
      blending: THREE.AdditiveBlending,
      depthWrite: false,
    });
    const mesh = new THREE.Mesh(explosionGeo, mat);
    mesh.position.copy(pos);
    mesh.scale.setScalar(scale * 0.4);
    group.add(mesh);
    explosions.push({ mesh, age: 0, life: 0.55, from: scale * 0.4, to: scale * 2.6 });
  }

  const SHIP_HIT_RADIUS = shipHitRadius;

  // Swept-sphere helper: did a bullet moving from prevPos to currPos hit a sphere?
  // Returns true if the segment passes within (obstRadius + RADIUS) of center.
  function sweptHit(prevX, prevY, prevZ, currX, currY, currZ, cx, cy, cz, obstRadius) {
    const dx = currX - prevX, dy = currY - prevY, dz = currZ - prevZ;
    const fx = prevX - cx, fy = prevY - cy, fz = prevZ - cz;
    const r = obstRadius + RADIUS;
    const a = dx * dx + dy * dy + dz * dz;
    if (a < 1e-12) return fx * fx + fy * fy + fz * fz < r * r;
    const b2 = fx * dx + fy * dy + fz * dz;
    const c = fx * fx + fy * fy + fz * fz - r * r;
    const disc = b2 * b2 - a * c;
    if (disc < 0) return false;
    const t = (-b2 - Math.sqrt(disc)) / a;
    return t >= 0 && t <= 1;
  }

  function update(dt, asteroids, remoteShips, onHitRemote, onHitAsteroid, shooterTeam, obstacles = []) {
    for (let i = bullets.length - 1; i >= 0; i--) {
      const b = bullets[i];
      const prevX = b.mesh.position.x, prevY = b.mesh.position.y, prevZ = b.mesh.position.z;
      b.mesh.position.addScaledVector(b.vel, dt);
      b.life -= dt;
      let consumed = b.life <= 0;

      // Asteroid collision: bullet pops with a small spark for instant
      // local feedback, but HP/destruction is server-authoritative — we
      // just report the hit and wait for `asteroid-hp`/`asteroid-destroyed`
      // broadcasts to mutate the field.
      if (!consumed && b.isLocal) {
        for (let j = asteroids.list.length - 1; j >= 0; j--) {
          const a = asteroids.list[j];
          const dx = b.mesh.position.x - a.mesh.position.x;
          const dy = b.mesh.position.y - a.mesh.position.y;
          const dz = b.mesh.position.z - a.mesh.position.z;
          const r = a.radius + RADIUS;
          if (dx * dx + dy * dy + dz * dz < r * r) {
            spawnExplosion(b.mesh.position, a.radius * 0.25);
            if (onHitAsteroid) onHitAsteroid(a.id);
            consumed = true;
            break;
          }
        }
      } else if (!consumed) {
        // Ghost bullets still get visually consumed by asteroid bodies so
        // they don't pass through. No callback, no server message — and
        // intentionally no spawnExplosion: 4 bots dogpiling a rock used
        // to flood low-end devices with additive transparent quads.
        for (let j = asteroids.list.length - 1; j >= 0; j--) {
          const a = asteroids.list[j];
          const dx = b.mesh.position.x - a.mesh.position.x;
          const dy = b.mesh.position.y - a.mesh.position.y;
          const dz = b.mesh.position.z - a.mesh.position.z;
          const r = a.radius + RADIUS;
          if (dx * dx + dy * dy + dz * dz < r * r) {
            consumed = true;
            break;
          }
        }
      }

      // Static obstacles (e.g. the moon). Use swept-sphere so fast bullets
      // can't tunnel through even a large obstacle in a single frame.
      if (!consumed) {
        for (let j = 0; j < obstacles.length; j++) {
          const o = obstacles[j];
          const cx = b.mesh.position.x, cy = b.mesh.position.y, cz = b.mesh.position.z;
          const dx = cx - o.pos.x, dy = cy - o.pos.y, dz = cz - o.pos.z;
          const r = o.radius + RADIUS;
          const point = dx * dx + dy * dy + dz * dz < r * r;
          const swept = !point && sweptHit(prevX, prevY, prevZ, cx, cy, cz, o.pos.x, o.pos.y, o.pos.z, o.radius);
          if (point || swept) {
            if (b.isLocal) spawnExplosion(b.mesh.position, 0.4);
            consumed = true;
            break;
          }
        }
      }

      if (!consumed && remoteShips && b.isLocal) {
        for (const [id, r] of remoteShips) {
          if (!r.alive) continue;
          if (shooterTeam !== undefined && r.team === shooterTeam) continue;
          const dx = b.mesh.position.x - r.ship.position.x;
          const dy = b.mesh.position.y - r.ship.position.y;
          const dz = b.mesh.position.z - r.ship.position.z;
          const reach = SHIP_HIT_RADIUS + RADIUS;
          if (dx * dx + dy * dy + dz * dz < reach * reach) {
            spawnExplosion(b.mesh.position, 1.0);
            if (onHitRemote) onHitRemote(id);
            consumed = true;
            break;
          }
        }
      }

      if (consumed) {
        group.remove(b.mesh);
        bullets.splice(i, 1);
      }
    }

    for (let i = explosions.length - 1; i >= 0; i--) {
      const e = explosions[i];
      e.age += dt;
      const t = e.age / e.life;
      const s = THREE.MathUtils.lerp(e.from, e.to, t);
      e.mesh.scale.setScalar(s);
      e.mesh.material.opacity = (1 - t) * 0.95;
      if (t >= 1) {
        group.remove(e.mesh);
        e.mesh.material.dispose();
        explosions.splice(i, 1);
      }
    }
  }

  return { group, fire, update, spawnExplosion };
}
