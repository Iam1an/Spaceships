import * as THREE from 'three';

export function createWarpEffect(scene, camera) {
  const WARP_DURATION = 1.5; // Quick mask during loading
  let warpTimer = WARP_DURATION;

  const starCount = 3000; // More lines
  const geometry = new THREE.BoxGeometry(0.4, 0.4, 1); // Thicker lines
  
  // Bright glowing material
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
  const phases = new Float32Array(starCount); // Random offset for organic feel

  for (let i = 0; i < starCount; i++) {
    const angle = Math.random() * Math.PI * 2;
    const radius = 10 + Math.random() * 200; // Tighter center, wider spread
    
    const x = Math.cos(angle) * radius;
    const y = Math.sin(angle) * radius;
    const z = (Math.random() - 0.5) * 1000;
    
    dummy.position.set(x, y, z);
    
    // Scale Z to make them look like lines, length based on speed later
    dummy.scale.set(1, 1, 1);
    dummy.updateMatrix();
    instancedMesh.setMatrixAt(i, dummy.matrix);
    
    velocities[i] = 2000 + Math.random() * 3000;
    phases[i] = Math.random();
  }
  
  instancedMesh.instanceMatrix.needsUpdate = true;
  scene.add(instancedMesh);

  const baseFov = camera.fov;
  // Extreme FOV to naturally cause a fish-eye / screen warp effect
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

      // Flash brightly at the start, fade out at the end
      const opacity = warpTimer < 0.8 ? (warpTimer / 0.8) : 
                     (progress < 0.2 ? (progress / 0.2) : 1.0);
      material.opacity = opacity;

      // Extreme screen warp effect using FOV
      // We keep it extremely high for the first half, then snap it back
      const fovProgress = 1.0 - Math.pow(1.0 - progress, 6); // Sharper ease-out holds the warp longer
      camera.fov = THREE.MathUtils.lerp(maxFov, baseFov, fovProgress);
      camera.updateProjectionMatrix();

      instancedMesh.position.copy(camera.position);
      instancedMesh.quaternion.copy(camera.quaternion);

      // We need to decode the matrices to update positions
      // A faster way is to just use a custom shader, but InstancedMesh is okay for 3000
      const speedMult = THREE.MathUtils.lerp(2.0, 0.05, progress);
      const position = new THREE.Vector3();
      const quaternion = new THREE.Quaternion();
      const scale = new THREE.Vector3();

      for (let i = 0; i < starCount; i++) {
        instancedMesh.getMatrixAt(i, dummy.matrix);
        dummy.matrix.decompose(position, quaternion, scale);

        const speed = velocities[i] * dt * speedMult;
        position.z += speed;

        // Wrap around from behind camera to far away
        if (position.z > 100) {
          position.z -= 1000;
        }

        // Stretch line based on speed so they look like thick laser bolts
        scale.z = Math.max(1, speed * 0.5 + 20);

        dummy.position.copy(position);
        dummy.scale.copy(scale);
        dummy.updateMatrix();
        instancedMesh.setMatrixAt(i, dummy.matrix);
      }
      
      instancedMesh.instanceMatrix.needsUpdate = true;
    }
  };
}
