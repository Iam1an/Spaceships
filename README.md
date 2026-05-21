# Spaceships

A multiplayer 3D space combat game built with Three.js and Node.js. Pilot your ship through asteroid fields, engage in dogfights with other players, and customize your vessel.

## Features

- **3D Space Combat** - Fully realized 3D environments with asteroids, moons, and a dynamic skybox
- **Multiplayer** - Real-time multiplayer gameplay using WebSockets
- **Ship Customization** - Personalize your spaceship with different colors and styles
- **Dynamic Physics** - Realistic flight mechanics with beams, bullets, and collision detection
- **Audio System** - Immersive sound effects for weapons, engines, and environmental audio
- **Mobile Support** - Touch-friendly HUD controls for mobile and tablet devices
- **User Accounts** - Secure authentication with password encryption to track pilot stats
- **AI Opponents** - Bot pilots to challenge you when playing solo

## Play Now

The game is hosted and ready to play at **[gheat.net/spaceships](https://gheat.net/spaceships)**

No installation required - just open the link in your browser and start piloting!

## Project Structure

```
├── src/                  # Client-side game code
│   ├── main.js          # Main game loop and initialization
│   ├── ship.js          # Ship model and mechanics
│   ├── asteroids.js     # Asteroid generation and physics
│   ├── bot.js           # AI bot pilots
│   ├── bullets.js       # Projectile system
│   ├── beams.js         # Beam weapons
│   ├── lobby.js         # Multiplayer lobby and matchmaking
│   ├── auth.js          # Client-side authentication
│   ├── customization.js # Ship customization UI
│   ├── input.js         # Keyboard/mouse controls
│   ├── touchhud.js      # Mobile touch controls
│   ├── camera.js        # Third-person camera system
│   ├── audio.js         # Sound effects and music
│   ├── skybox.js        # Space background rendering
│   ├── trails.js        # Engine trail effects
│   ├── moon.js          # Moon object and rendering
│   └── mothership.js    # Static mothership object
├── server/              # Backend server code
│   ├── index.js         # Express server and WebSocket handler
│   └── db.js            # SQLite database interface
├── public/              # Static assets (images, sounds, etc.)
├── index.html           # Main HTML entry point
└── pilots.db            # SQLite database (user accounts)
```

## Gameplay

### Controls

**Keyboard & Mouse:**
- **W** - Thrust forward
- **A/D** - Roll left/right
- **Q/E** - Pitch up/down
- **Mouse** - Aim and look around
- **Left Click** - Fire bullets
- **Right Click** - Fire beam weapon
- **Shift** - Boost
- **Space** - Drift

**Touch Controls:**
- Virtual joystick for movement
- Aim crosshair with finger
- On-screen buttons for weapons

### Game Modes

- **Multiplayer** - Join lobbies and compete against other players in real-time
- **Solo** - Practice against AI bots or explore the asteroids
- **Customization** - Personalize your ship's appearance before battle

## Features in Detail

### Ship Customization
Create a unique ship by selecting from various color schemes and visual styles. Your customization is saved to your pilot account.

### Physics System
Ships respond realistically to input with proper acceleration, momentum, and rotation. Collisions with asteroids and other ships have consequences.

### Weapons
- **Bullets** - Fast projectiles with limited range
- **Beams** - Sustained energy weapons with overheating

### Audio
Dynamic audio system with effects for thrusters, weapon fire, impacts, and ambient space sounds.

### Multiplayer
Uses WebSocket connections for real-time synchronization of ship positions, rotations, and combat events across all connected players.

## Technology Stack

- **Frontend:**
  - Three.js - 3D graphics and rendering
  - HTML5 Canvas - Game rendering
  - JavaScript (ES6 modules)

- **Backend:**
  - Express.js - Web server
  - WebSocket (ws) - Real-time multiplayer communication
  - SQLite3 / better-sqlite3 - Data persistence
  - bcrypt - Secure password hashing
  - JWT - Authentication tokens

## Database

The game uses SQLite to store:
- Pilot accounts and login credentials
- Ship customization preferences
- Game stats and progression

Database file: `pilots.db`

## Development

### Key Components

- **main.js** - Game loop, rendering pipeline, and game state management
- **ship.js** - Ship movement, rotation, and physics
- **bot.js** - AI pathfinding and combat logic
- **lobby.js** - Multiplayer matchmaking and lobby system
- **server/index.js** - Express routes, WebSocket message handling, game server logic

### Adding New Features

1. Client-side features go in `src/`
2. Server-side features and game logic in `server/`
3. Update the protocol in both locations to keep them in sync

## Performance

The game is optimized for:
- 60 FPS gameplay
- Efficient particle effects and trails
- Frustum culling for asteroids
- WebSocket message batching for multiplayer sync

## License

This game and all its code, assets, and design are the exclusive property of the creators. Unauthorized copying, reproduction, distribution, or use of this game in any form is strictly prohibited. Do not copy or steal this game.

## Credits

- **Ian** - Creator of the game
- **Gheat** - Online play and backend infrastructure

---

**Ready to pilot? Launch the game and dominate the asteroid field!**
