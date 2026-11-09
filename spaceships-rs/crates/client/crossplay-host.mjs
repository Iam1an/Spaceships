// Drives the browser half of a cross-play check.
//
//   node crossplay-host.mjs <outDir> [url]
//
// Playwright only: every key, wheel notch and pointer move here is a CDP event
// delivered to the page this script launched. Nothing synthesises OS input, so
// nothing can land in whatever the person at the keyboard is doing. The native
// half is started separately with `SPACESHIPS_ROOM=<code>` and flies nothing —
// see `net::tests::a_rust_peer_kills_a_browser_peer_through_the_js_server` for
// the half of the check that needs no window at all.
//
// Guest login, create an empty room, print `CODE <ABCD>` (start the Rust client
// with `SPACESHIPS_ROOM=<ABCD>`), wait for a second pilot, launch, then fly at
// them and hold the trigger, screenshotting as it goes.
//
// Steering is `page.mouse.move`: without pointer lock — which synthetic input
// cannot take — `public/src/input.js` reads the cursor's *absolute* position
// relative to the centre of the window as the stick, so an (x, y) is a stick
// deflection. That is what makes the browser side the one that manoeuvres: the
// moon sits at the origin, exactly between the two spawns, and somebody has to
// go over it.
//
// Headed on purpose: headless Chromium falls back to SwiftShader (software GL)
// and the frame it captures is not the frame a player would see.
import { chromium } from 'playwright';

const outDir = process.argv[2] || '/tmp';
const url = process.argv[3] || 'http://localhost:4000';
const W = 1280;
const H = 800;

const browser = await chromium.launch({ headless: false });
const page = await browser.newPage({ viewport: { width: W, height: H } });

await page.goto(url);
await page.click('#auth-guest-btn');
await page.waitForSelector('#lobby-main:not(.hidden)');
await page.click('#btnMulti');
await page.click('#btnCreateMenu');
// The toggle CLAUDE.md tells testers to clear: no balance bot, so the only two
// ships on the map are the two clients under test.
await page.uncheck('#autoBotInput');
await page.click('#btnPlay');
await page.waitForSelector('#lobby-room:not(.hidden)');

console.log('CODE ' + (await page.textContent('#roomCode')).trim());

await page.waitForFunction(
  () => document.querySelectorAll('#players li').length >= 2,
  null,
  { timeout: 180000 },
);
console.log('ROSTER ' + (await page.textContent('#players')).replace(/\s+/g, ' ').trim());

await page.click('#btnStart');
await page.waitForTimeout(2200);
console.log('LAUNCHED');

// The scoreboard, while both pilots are still on their spawns. This is the
// frame that shows one match with two clients in it — the native half captures
// itself at the same moment with its own `SPACESHIPS_SCREENSHOT` timer.
await page.keyboard.down('Tab');
await page.waitForTimeout(400);
await page.screenshot({ path: `${outDir}/roster.png` });
console.log('SHOT ' + outDir + '/roster.png');
await page.keyboard.up('Tab');

const stick = (x, y) => page.mouse.move(W / 2 + x, H / 2 + y);
// Throttle with the wheel, not by holding `W`.
//
// The native client has to be the frontmost window for its own synthetic keys
// to land, and Chromium clears every held key on `blur`
// (`public/src/input.js`) — so a held `W` here dies the moment the game window
// comes forward. The wheel is a *latched* throttle
// (`THROTTLE_STEP = 6 u/s` a notch) and the pointer position is absolute, so
// both survive losing focus. Firing is a discrete press and survives too.
const throttle = async (notches) => {
  for (let i = 0; i < Math.abs(notches); i++) {
    await page.mouse.wheel(0, notches > 0 ? -100 : 100);
    await page.waitForTimeout(30);
  }
};

await stick(0, 0);
await throttle(14); // 14 x 6 u/s clamps at MAX_THROTTLE

// Climb just enough to clear the moon (radius 80 at the origin), then level.
await stick(0, -150);
await page.waitForTimeout(450);
await stick(0, 0);

let shot = 0;
const grab = async (tag) => {
  const path = `${outDir}/browser-${String(shot++).padStart(2, '0')}-${tag}.png`;
  await page.screenshot({ path });
  console.log('SHOT ' + path);
};

// Nose over onto the far spawn and hold the trigger.
for (let step = 0; step < 30; step++) {
  // Nose over onto the far spawn, then bleed speed so the pass becomes a
  // stand-off rather than an overshoot.
  if (step === 5) await stick(0, 120);
  if (step === 7) await stick(0, 0);
  if (step === 8) await throttle(-10); // bleed speed into a stand-off
  for (let i = 0; i < 12; i++) {
    await page.keyboard.press('f');
    await page.waitForTimeout(100);
  }
  await grab(String(step));
}
console.log('DONE');
await page.waitForTimeout(3000);
await browser.close();
