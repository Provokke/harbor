// Macrotasks (timers) vs microtasks ordering.
const log = [];
setTimeout(() => {
  log.push("t1");
  Promise.resolve().then(() => log.push("t1-micro"));
  queueMicrotask(() => log.push("t1-qm"));
}, 0);
setTimeout(() => log.push("t2"), 0);
setTimeout(() => log.push("t-25ms"), 25);
setTimeout(() => log.push("t-20ms"), 20);
const cancelled = setTimeout(() => log.push("NEVER"), 1);
clearTimeout(cancelled);
queueMicrotask(() => log.push("qm1"));
Promise.resolve().then(() => log.push("p1")).then(() => log.push("p2"));
log.push("sync");
let n = 0;
const iv = setInterval((a, b) => {
  n++;
  log.push(`iv${n}:${a}${b}`);
  if (n === 3) clearInterval(iv);
}, 1, "x", "y");
setTimeout(() => {
  console.log(log.join(","));
  // timer args + nested timers
  setTimeout((x) => console.log("nested", x), 0, 99);
}, 40);
const t0 = performance.now();
setTimeout(() => {
  const dt = performance.now() - t0;
  console.log("50ms timer waited >= 50ms:", dt >= 49);
}, 50);
