// Op-call overhead and file read throughput. Run: harbor run --allow-read --allow-write benches/ops.js <tmpdir>
const { noop, noopAsync, bufAddr } = harbor.core.ops;
const now = () => performance.now();
function rate(n, ms) { return Math.round(n / (ms / 1000)).toLocaleString?.() ?? String(Math.round(n / (ms / 1000))); }
function report(name, n, ms, unit = "ops/s") { console.log(`${name.padEnd(34)} ${String(Math.round(n / (ms / 1000))).padStart(12)} ${unit}  (${(ms * 1e6 / n).toFixed(0)} ns/op)`); }

// warm up
for (let i = 0; i < 100000; i++) noop();
let N = 5_000_000, t = now();
for (let i = 0; i < N; i++) noop();
report("sync op (noop)", N, now() - t);

const buf = new Uint8Array(1 << 20);
N = 2_000_000; t = now();
for (let i = 0; i < N; i++) bufAddr(buf);
report("sync op w/ 1MB Uint8Array arg", N, now() - t);

N = 200_000; t = now();
for (let i = 0; i < N; i++) await noopAsync();
report("async op (sequential await)", N, now() - t);

N = 200_000; t = now();
const batch = 1000;
for (let i = 0; i < N / batch; i++) await Promise.all(Array.from({ length: batch }, noopAsync));
report("async op (1000 in flight)", N, now() - t);

const dir = harbor.args[0];
const size = 256 * 1024 * 1024;
const file = `${dir}/harbor-bench.bin`;
await harbor.writeFile(file, new Uint8Array(size));
t = now();
const data = await harbor.readFile(file);
let ms = now() - t;
console.log(`${"readFile 256MB (zero-copy handoff)".padEnd(34)} ${(size / (ms / 1000) / 1e9).toFixed(2).padStart(12)} GB/s  (${ms.toFixed(1)} ms)`);
const h = await harbor.open(file);
const chunk = new Uint8Array(1 << 20);
let total = 0;
t = now();
while (true) { const n = await h.read(chunk); if (n === null) break; total += n; }
ms = now() - t;
h.close();
console.log(`${"read() 1MB chunks into JS buffer".padEnd(34)} ${(total / (ms / 1000) / 1e9).toFixed(2).padStart(12)} GB/s  (${ms.toFixed(1)} ms)`);
t = now();
let total2 = 0;
const h2 = await harbor.open(file);
for await (const c of h2.readable) total2 += c.length;
ms = now() - t;
console.log(`${"readable stream (64KB chunks)".padEnd(34)} ${(total2 / (ms / 1000) / 1e9).toFixed(2).padStart(12)} GB/s  (${ms.toFixed(1)} ms)`);
await harbor.remove(file);
if (data.length !== size || total !== size || total2 !== size) throw new Error("size mismatch");
