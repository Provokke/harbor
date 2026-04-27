import { b, bLate } from "./cycle_b.js";
export const a = "A";
export function aFn() { return `aFn sees ${b}`; }
export function callB() { return bLate(); }
console.log("eval cycle_a (b already evaluated:", typeof b === "string", ")");
