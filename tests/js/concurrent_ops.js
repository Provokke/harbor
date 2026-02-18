// args: --allow-read --allow-write=$TMP
import { assertEq } from "./_assert.js";
const dir = harbor.args[0];
// 100 files written and read back concurrently
await Promise.all(Array.from({ length: 100 }, (_, i) => harbor.writeTextFile(`${dir}/f${i}.txt`, `content-${i}`)));
const texts = await Promise.all(Array.from({ length: 100 }, (_, i) => harbor.readTextFile(`${dir}/f${i}.txt`)));
texts.forEach((t, i) => assertEq(t, `content-${i}`));
console.log("100 concurrent fs round-trips ok");
// 10k async ops in flight at once
const results = await Promise.all(Array.from({ length: 10000 }, () => harbor.core.ops.noopAsync()));
console.log("10000 concurrent async ops settled:", results.length);
// ops and timers interleave; a slow timer does not block op completion
const order = [];
const slow = new Promise((r) => setTimeout(() => { order.push("timer20"); r(); }, 20));
const fast = harbor.readTextFile(`${dir}/f1.txt`).then(() => order.push("read"));
await Promise.all([slow, fast]);
console.log(order.join(" < "));
// Promise.race between op and timer
const winner = await Promise.race([
  harbor.core.ops.noopAsync().then(() => "op"),
  new Promise((r) => setTimeout(() => r("timer"), 50)),
]);
console.log("race winner:", winner);
// mixed failures are isolated per promise
const settled = await Promise.allSettled([harbor.readTextFile(`${dir}/f2.txt`), harbor.readTextFile(`${dir}/nope`), harbor.writeTextFile("/forbidden", "x")]);
console.log(settled.map((s) => s.status === "fulfilled" ? s.value : s.reason.name).join(","));
