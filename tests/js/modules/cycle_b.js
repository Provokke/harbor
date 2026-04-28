import { a, aFn } from "./cycle_a.js";
export const b = "B";
export function bLate() { return `bLate sees ${a} and ${aFn()}`; }
let tdz = "unset";
try { tdz = a; } catch (e) { tdz = e.constructor.name; }
console.log("eval cycle_b, a during cycle ->", tdz);
