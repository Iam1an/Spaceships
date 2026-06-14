import * as THREE from 'three';
export function createWarpEffect(scene, camera) {
  const WARP_DURATION = 1.5;
  let warpTimer = WARP_DURATION;
  const starCount = 3000;
  const geometry = new THREE.BoxGeometry(0.4, 0.4, 1);
  const material = new THREE.MeshBasicMaterial({
    color: 0xccffff,
    transparent: true,
    opacity: 1.0,
    blending: THREE.AdditiveBlending,
    depthWrite: false
  });
  const instancedMesh = new THREE.InstancedMesh(geometry, material, starCount);
  instancedMesh.instanceMatrix.setUsage(THREE.DynamicDrawUsage);
  const dummy = new THREE.Object3D();
  const velocities = new Float32Array(starCount);
  const posX = new Float32Array(starCount);
  const posY = new Float32Array(starCount);
  const posZ = new Float32Array(starCount);
  for (let i = 0; i < starCount; i++) {
    const angle = Math.random() * Math.PI * 2;
    const radius = 10 + Math.random() * 200;
    const x = Math.cos(angle) * radius;
    const y = Math.sin(angle) * radius;
    const z = (Math.random() - 0.5) * 1000;
    posX[i] = x;
    posY[i] = y;
    posZ[i] = z;
    dummy.position.set(x, y, z);
    dummy.scale.set(1, 1, 1);
    dummy.updateMatrix();
    instancedMesh.setMatrixAt(i, dummy.matrix);
    velocities[i] = 2000 + Math.random() * 3000;
  }
  instancedMesh.instanceMatrix.needsUpdate = true;
  scene.add(instancedMesh);
  const baseFov = camera.fov;
  const maxFov = 175;
  camera.fov = maxFov;
  camera.updateProjectionMatrix();
  return {
    update: (dt) => {
      if (warpTimer <= 0) {
        if (instancedMesh.visible) {
          instancedMesh.visible = false;
          camera.fov = baseFov;
          camera.updateProjectionMatrix();
        }
        return;
      }
      warpTimer -= dt;
      let progress = 1.0 - (warpTimer / WARP_DURATION);
      if (progress < 0) progress = 0;
      if (progress > 1) progress = 1;
      const opacity = warpTimer < 0.8 ? (warpTimer / 0.8) :
        (progress < 0.2 ? (progress / 0.2) : 1.0);
      material.opacity = opacity;
      const fovProgress = 1.0 - Math.pow(1.0 - progress, 6);
      camera.fov = THREE.MathUtils.lerp(maxFov, baseFov, fovProgress);
      camera.updateProjectionMatrix();
      instancedMesh.position.copy(camera.position);
      instancedMesh.quaternion.copy(camera.quaternion);
      const speedMult = THREE.MathUtils.lerp(2.0, 0.05, progress);
      for (let i = 0; i < starCount; i++) {
        const speed = velocities[i] * dt * speedMult;
        posZ[i] += speed;
        if (posZ[i] > 100) {
          posZ[i] -= 1000;
        }
        dummy.position.set(posX[i], posY[i], posZ[i]);
        dummy.scale.set(1, 1, Math.max(1, speed * 0.5 + 20));
        dummy.updateMatrix();
        instancedMesh.setMatrixAt(i, dummy.matrix);
      }
      instancedMesh.instanceMatrix.needsUpdate = true;
    }
  };
}
