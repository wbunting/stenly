import { spawn, spawnSync } from 'node:child_process';

const zola = process.env.ZOLA_BIN || 'zola';
const build = spawnSync('npm', ['run', 'css'], { stdio: 'inherit' });
if (build.status !== 0) process.exit(build.status || 1);
const children = [
  spawn('npm', ['run', 'css:watch'], { stdio: 'inherit' }),
  spawn(zola, ['serve', '--interface', '127.0.0.1', '--port', process.env.PORT || '1111'], { stdio: 'inherit' }),
];
let stopping = false;
function stop(code = 0) {
  if (stopping) return;
  stopping = true;
  children.forEach((child) => child.kill('SIGTERM'));
  process.exitCode = code;
}
children.forEach((child) => {
  child.on('error', (error) => { console.error(error.message); stop(1); });
  child.on('exit', (code) => { if (!stopping) stop(code || 0); });
});
process.on('SIGINT', () => stop());
process.on('SIGTERM', () => stop());
