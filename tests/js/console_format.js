// stderr: to stderr
console.log("str", 1, -0, 2n, true, null, undefined, Symbol("s"));
console.log([1, [2, [3]]], { a: 1, "b-c": "x", nested: { d: [] } });
console.log(new Set([1, "a"]), new Map([["k", { v: 1 }]]));
console.log(new Uint8Array([1, 2, 3]), function named() {}, () => {});
console.log("%s is %d years and %i%%", "Bob", 42.5, 42.9, "extra");
const circ = { name: "c" }; circ.self = circ;
console.log(circ);
class Point { constructor() { this.x = 1; this.y = 2; } }
console.log(new Point());
console.error("to stderr");
console.group("group");
console.log("indented");
console.groupEnd();
console.count(); console.count(); console.count("x");
console.log(new Error("shown").message);
