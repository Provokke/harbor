// args: (none) — every read-category op must be denied by default
import { assertRejects, assertThrows } from "./_assert.js";
const PD = harbor.errors.PermissionDenied;
const p = "/etc/hostname";
const checks = {
  readTextFile: () => harbor.readTextFile(p),
  readFile: () => harbor.readFile(p),
  stat: () => harbor.stat(p),
  open: () => harbor.open(p),
};
for (const [name, fn] of Object.entries(checks)) {
  const e = await assertRejects(fn, PD, name);
  console.log(name, "->", e.name);
}
let e = assertThrows(() => harbor.readTextFileSync(p), PD);
console.log("readTextFileSync ->", e.message);
e = assertThrows(() => harbor.cwd(), PD);
console.log("cwd ->", e.name);
console.log(e instanceof Error, e.stack.split("\n")[0]);
