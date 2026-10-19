import { assert, assertEq } from "./_assert.js";
const { alloc, lastAllocAddr, bufAddr, bufFill, bufSum } = harbor.core.ops;

// JS -> Rust: Rust sees JS writes, JS sees Rust writes, same memory.
const a = new Uint8Array(16);
a[0] = 200; a[15] = 55;
assertEq(bufSum(a), 255, "rust sees js writes");
bufFill(a, 1, 1);
assertEq(Array.from(a.subarray(0, 4)), [1, 2, 3, 4], "js sees rust writes");
// a subarray view shares the parent's memory, offset by byteOffset
const view = a.subarray(4, 8);
assertEq(bufAddr(view) - bufAddr(a), 4, "subarray address offset");
bufFill(view, 0, 0);
assertEq(Array.from(a.subarray(2, 10)), [3, 4, 0, 0, 0, 0, 9, 10], "write through view");
console.log("bidirectional mutation ok");

// Rust -> JS: the Uint8Array is backed by the very allocation Rust made.
const r = alloc(1024, 9);
assertEq(bufAddr(r), lastAllocAddr(), "Rust Vec handed over without copy");
r[3] = 1;
assertEq(bufSum(r), 1023 * 9 + 1);
console.log("rust-allocated buffer adopted without copy");

// 100 MB: a copy would take tens of ms and double RSS; a pointer hand-off is O(1).
const MB = 1024 * 1024;
let t = performance.now();
const big = alloc(100 * MB, 1);
const allocMs = performance.now() - t;
assertEq(bufAddr(big), lastAllocAddr(), "100MB not copied");
t = performance.now();
for (let i = 0; i < 1000; i++) bufAddr(big);
const passMs = (performance.now() - t) / 1000;
big[100 * MB - 1] = 2;
t = performance.now();
const sum = bufSum(big);
const sumMs = performance.now() - t;
assertEq(sum, 100 * MB + 1, "rust reads all 100MB incl. JS write at the end");
const jsBig = new Uint8Array(100 * MB);
t = performance.now();
bufFill(jsBig, 0, 1);
const fillMs = performance.now() - t;
assertEq(jsBig[100 * MB - 1], (100 * MB - 1) & 0xff);
assert(passMs < 1, `passing 100MB to an op should be ~free, took ${passMs}ms`);
console.log("100MB zero-copy ok");
console.error(`[measure] 100MB: alloc+handoff ${allocMs.toFixed(2)}ms, pass-to-op ${(passMs * 1000).toFixed(2)}us/call, rust sum ${sumMs.toFixed(2)}ms, rust fill ${fillMs.toFixed(2)}ms`);
